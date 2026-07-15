using System.Globalization;
using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;
using QsoRipper.Engine.Storage;

#pragma warning disable CA1031 // Do not catch general exception types — sync must be resilient to partial failures

namespace QsoRipper.Engine.QrzLogbook;

/// <summary>
/// Orchestrates a 3-phase bidirectional sync between the local logbook and QRZ:
/// <list type="number">
///   <item><description>Download remote QSOs and merge/insert locally.</description></item>
///   <item><description>Upload pending local QSOs to QRZ.</description></item>
///   <item><description>Update sync metadata with the current timestamp.</description></item>
/// </list>
/// </summary>
public sealed class QrzSyncEngine
{
    /// <summary>Extra-field key that QRZ ADIF uses for the logbook record ID.</summary>
    private const string QrzLogidExtraField = "APP_QRZ_LOGID";

    /// <summary>Alternate extra-field key used by some QRZ ADIF exports.</summary>
    private const string QrzLogidAltExtraField = "APP_QRZLOG_LOGID";

    /// <summary>Maximum time difference (seconds) for fuzzy timestamp matching.</summary>
    private const long TimestampToleranceSeconds = 60;

    private readonly IQrzLogbookApi _client;

    /// <summary>
    /// Initializes a new instance of the <see cref="QrzSyncEngine"/> class.
    /// </summary>
    public QrzSyncEngine(IQrzLogbookApi client)
    {
        ArgumentNullException.ThrowIfNull(client);
        _client = client;
    }

    /// <summary>
    /// Upload one QSO for the per-operation LogQso or UpdateQso path.
    /// The caller persists the returned QRZ logid with its local write.
    /// </summary>
    public async Task<string> UploadSingleQsoAsync(ILogbookStore store, QsoRecord qso)
    {
        ArgumentNullException.ThrowIfNull(store);
        ArgumentNullException.ThrowIfNull(qso);

        var metadata = await store.GetSyncMetadataAsync().ConfigureAwait(false);
        string? bookOwner = null;
        try
        {
            var status = await _client.GetStatusAsync().ConfigureAwait(false);
            if (!string.IsNullOrWhiteSpace(status.Owner))
            {
                bookOwner = status.Owner.Trim();
            }
        }
        catch (QrzLogbookException)
        {
            bookOwner = metadata.QrzLogbookOwner;
        }

        var hasExistingLogid = !string.IsNullOrWhiteSpace(qso.QrzLogid);
        if (qso.SyncStatus == SyncStatus.Modified && hasExistingLogid)
        {
            return await _client.UpdateQsoAsync(qso, bookOwner).ConfigureAwait(false);
        }

        try
        {
            return await _client.UploadQsoAsync(qso, bookOwner).ConfigureAwait(false);
        }
        catch (QrzLogbookException ex) when (!hasExistingLogid && IsDuplicateError(ex.Message))
        {
            return await _client.UploadQsoWithReplaceAsync(qso, bookOwner).ConfigureAwait(false);
        }
    }

    /// <summary>Delete one QRZ row before a destructive local purge.</summary>
    public Task DeleteRemoteQsoAsync(string logid)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(logid);
        return _client.DeleteQsoAsync(logid.Trim());
    }

    /// <summary>
    /// Execute a full sync cycle against the given logbook store.
    /// </summary>
    /// <param name="store">The logbook store to sync.</param>
    /// <param name="fullSync">When <c>true</c>, re-fetches all QRZ records instead of incremental.</param>
    /// <param name="conflictPolicy">
    /// How to resolve QSOs modified locally and remotely since the last sync.
    /// <c>CONFLICT_POLICY_UNSPECIFIED</c> is treated as <c>FLAG_FOR_REVIEW</c>
    /// per the engine spec (safe/non-destructive default).
    /// </param>
    /// <returns>A <see cref="SyncResult"/> with counts and any error summary.</returns>
    public async Task<SyncResult> ExecuteSyncAsync(
        ILogbookStore store,
        bool fullSync,
        ConflictPolicy conflictPolicy = ConflictPolicy.Unspecified)
    {
        ArgumentNullException.ThrowIfNull(store);

        // Treat the proto zero-value as the safe default per engine spec §6.3.
        var effectivePolicy = conflictPolicy == ConflictPolicy.Unspecified
            ? ConflictPolicy.FlagForReview
            : conflictPolicy;

        var errors = new List<string>();
        uint downloaded = 0;
        uint uploaded = 0;
        uint conflicts = 0;
        uint remoteDeletesPushed = 0;
        uint deletesSkippedRemote = 0;
        uint duplicateReplaces = 0;

        SyncMetadata metadata;
        try
        {
            metadata = await store.GetSyncMetadataAsync().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            metadata = new SyncMetadata();
            errors.Add($"Failed to read sync metadata: {ex.Message}");
        }

        // ---------------------------------------------------------------
        // Phase 0.5 — Resolve owner and push local corrections first
        // ---------------------------------------------------------------
        // Under the default safe conflict policy, a stale QRZ copy of a locally
        // modified row would otherwise be downloaded first and mark the row
        // Conflict before Phase 2 can upload it. Push QRZ-linked local edits
        // before download so F6 sync updates the existing QRZ row.

        QrzLogbookStatus? statusResult = null;
        Exception? statusException = null;
        try
        {
            statusResult = await _client.GetStatusAsync().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            statusException = ex;
        }

        string? bookOwner = null;
        if (statusResult is { } sr && !string.IsNullOrWhiteSpace(sr.Owner))
        {
            bookOwner = sr.Owner.Trim();
        }
        else if (!string.IsNullOrWhiteSpace(metadata.QrzLogbookOwner))
        {
            // STATUS failed or returned no owner — fall back to the cached
            // owner so QSOs can still upload after a transient API hiccup.
            bookOwner = metadata.QrzLogbookOwner;
        }

        var freshlyUploadedLogids = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        if (effectivePolicy != ConflictPolicy.LastWriteWins)
        {
            var preUpload = await UploadPendingQsosAsync(
                store,
                bookOwner,
                q => q.SyncStatus == SyncStatus.Modified && !string.IsNullOrWhiteSpace(q.QrzLogid),
                errors).ConfigureAwait(false);
            uploaded += preUpload.Uploaded;
            duplicateReplaces += preUpload.DuplicateReplaces;
            freshlyUploadedLogids.UnionWith(preUpload.UploadedLogids);
        }

        // ---------------------------------------------------------------
        // Phase 1 — Download from QRZ
        // ---------------------------------------------------------------

        IReadOnlyList<QsoRecord> localQsosAll;
        try
        {
            // Load active + soft-deleted rows so we can both match against
            // active rows AND skip remote downloads whose qrz_logid belongs
            // to a soft-deleted local row (otherwise trashed QSOs would be
            // resurrected on the next sync).
            localQsosAll = await store.ListQsosAsync(new QsoListQuery
            {
                Sort = QsoSortOrder.OldestFirst,
                DeletedFilter = DeletedRecordsFilter.All,
            }).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            return new SyncResult
            {
                ErrorSummary = $"Failed to load local QSOs: {ex.Message}",
            };
        }

        var deletedLogids = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var localQsos = new List<QsoRecord>(localQsosAll.Count);
        foreach (var qso in localQsosAll)
        {
            if (qso.DeletedAt is not null)
            {
                var logid = ExtractQrzLogid(qso);
                if (!string.IsNullOrWhiteSpace(logid))
                {
                    deletedLogids.Add(logid);
                }
            }
            else
            {
                localQsos.Add(qso);
            }
        }

        // Force full fetch when local logbook is empty (first sync or data loss recovery).
        var sinceDate = (fullSync || localQsos.Count == 0) ? null : FormatSinceDate(metadata);

        List<QsoRecord> remoteQsos;
        try
        {
            remoteQsos = await _client.FetchQsosAsync(sinceDate).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            return new SyncResult
            {
                ErrorSummary = $"Failed to fetch QSOs from QRZ: {ex.Message}",
            };
        }

        // Build lookup indexes over local QSOs.
        var (byLogid, byKey) = BuildLocalIndexes(localQsos);

        foreach (var remote in remoteQsos)
        {
            // Ghost filter: skip QSOs with empty callsign or missing timestamp.
            if (string.IsNullOrWhiteSpace(remote.WorkedCallsign) || remote.UtcTimestamp is null)
            {
                continue;
            }

            var remoteLogid = ExtractQrzLogid(remote);

            // If this sync just pushed the local correction to QRZ, ignore a
            // stale remote copy returned by the same FETCH response. QRZ may
            // lag briefly after REPLACE; local storage already reflects the
            // operator's corrected record.
            if (remoteLogid is not null && freshlyUploadedLogids.Contains(remoteLogid))
            {
                continue;
            }

            // Phase 1 skip: don't resurrect soft-deleted local rows.
            if (remoteLogid is not null && deletedLogids.Contains(remoteLogid))
            {
                deletesSkippedRemote++;
                continue;
            }

            // Try match by QRZ logid first, then fuzzy match.
            var localMatch = remoteLogid is not null && byLogid.TryGetValue(remoteLogid, out var logidMatch)
                ? logidMatch
                : FuzzyMatch(remote, byKey);

            if (localMatch is null)
            {
                // New remote QSO — insert locally.
                var newQso = remote.Clone();
                if (string.IsNullOrEmpty(newQso.LocalId))
                {
                    newQso.LocalId = Guid.NewGuid().ToString();
                }

                newQso.SyncStatus = SyncStatus.Synced;
                if (remoteLogid is not null)
                {
                    newQso.QrzLogid = remoteLogid;
                }

                try
                {
                    await store.InsertQsoAsync(newQso).ConfigureAwait(false);
                    downloaded++;

                    // Keep indexes current for subsequent iterations.
                    if (remoteLogid is not null)
                    {
                        byLogid[remoteLogid] = newQso;
                    }

                    var key = MakeFuzzyKey(newQso);
                    if (!byKey.TryGetValue(key, out var list))
                    {
                        list = [];
                        byKey[key] = list;
                    }

                    list.Add(newQso);
                }
                catch (Exception ex)
                {
                    errors.Add($"Insert failed for {remote.WorkedCallsign}: {ex.Message}");
                }
            }
            else
            {
                // Matched existing QSO — merge using the requested conflict policy.
                try
                {
                    var (merged, isConflict) =
                        MergeRemoteIntoLocal(localMatch, remote, remoteLogid, effectivePolicy);
                    if (isConflict)
                    {
                        conflicts++;
                    }

                    if (await store.UpdateQsoAsync(merged).ConfigureAwait(false))
                    {
                        downloaded++;
                    }
                }
                catch (Exception ex)
                {
                    errors.Add($"Merge failed for {remote.WorkedCallsign}: {ex.Message}");
                }
            }
        }

        // ---------------------------------------------------------------
        // Phase 2 — Upload pending local QSOs
        // ---------------------------------------------------------------

        var pendingUpload = await UploadPendingQsosAsync(
            store,
            bookOwner,
            q => q.SyncStatus is SyncStatus.LocalOnly or SyncStatus.Modified,
            errors).ConfigureAwait(false);
        uploaded += pendingUpload.Uploaded;
        duplicateReplaces += pendingUpload.DuplicateReplaces;

        // ---------------------------------------------------------------
        // Phase 2.5 — Push queued remote deletes to QRZ
        // ---------------------------------------------------------------

        IReadOnlyList<QsoRecord> pendingRemoteDeletes;
        try
        {
            var deletedRows = await store.ListQsosAsync(new QsoListQuery
            {
                Sort = QsoSortOrder.OldestFirst,
                DeletedFilter = DeletedRecordsFilter.DeletedOnly,
            }).ConfigureAwait(false);
            pendingRemoteDeletes = deletedRows
                .Where(q => q.PendingRemoteDelete && !string.IsNullOrWhiteSpace(q.QrzLogid))
                .ToList();
        }
        catch (Exception ex)
        {
            errors.Add($"Failed to list deleted QSOs for remote-delete pass: {ex.Message}");
            pendingRemoteDeletes = [];
        }

        foreach (var qso in pendingRemoteDeletes)
        {
            try
            {
                await _client.DeleteQsoAsync(qso.QrzLogid).ConfigureAwait(false);

                // Success (including QRZ "not found"): clear local pending
                // flags but keep the deleted_at tombstone so the row stays
                // in the trash view.
                var cleared = qso.Clone();
                cleared.QrzLogid = string.Empty;
                cleared.PendingRemoteDelete = false;
                try
                {
                    await store.UpdateQsoAsync(cleared).ConfigureAwait(false);
                    remoteDeletesPushed++;
                }
                catch (Exception ex)
                {
                    errors.Add($"Remote delete cleared for {qso.WorkedCallsign} failed locally: {ex.Message}");
                }
            }
            catch (Exception ex)
            {
                errors.Add($"Remote delete failed for {qso.WorkedCallsign}: {ex.Message}");
            }
        }

        // ---------------------------------------------------------------
        // Phase 3 — Refresh metadata from authoritative QRZ STATUS
        // ---------------------------------------------------------------
        // Reuses the STATUS call made in Phase 1.5 (so each sync makes one
        // STATUS round-trip, not two). Mirrors
        // src/rust/qsoripper-server/src/sync.rs::execute_sync: prefer remote
        // STATUS; on failure fall back to estimating from local counts so
        // metadata stays at least approximately correct.

        uint? remoteCount = null;
        string? remoteOwner = null;
        if (statusResult is { } status)
        {
            remoteCount = status.QsoCount;
            remoteOwner = string.IsNullOrWhiteSpace(status.Owner)
                ? metadata.QrzLogbookOwner
                : status.Owner;
        }
        else
        {
            errors.Add($"STATUS refresh failed: {statusException?.Message ?? "unknown"}");
            remoteOwner = metadata.QrzLogbookOwner;
        }

        int qrzCountToPersist;
        if (remoteCount is { } rc)
        {
            qrzCountToPersist = (int)Math.Min(rc, (uint)int.MaxValue);
        }
        else
        {
            try
            {
                var counts = await store.GetCountsAsync().ConfigureAwait(false);
                qrzCountToPersist = Math.Max(0, counts.LocalQsoCount - counts.PendingUploadCount);
            }
            catch (Exception ex)
            {
                errors.Add($"Local count refresh failed: {ex.Message}");
                qrzCountToPersist = metadata.QrzQsoCount;
            }
        }

        try
        {
            await store.UpsertSyncMetadataAsync(new SyncMetadata
            {
                QrzQsoCount = qrzCountToPersist,
                LastSync = DateTimeOffset.UtcNow,
                QrzLogbookOwner = remoteOwner,
            }).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            errors.Add($"Failed to update sync metadata: {ex.Message}");
        }

        return new SyncResult
        {
            DownloadedCount = downloaded,
            UploadedCount = uploaded,
            ConflictCount = conflicts,
            RemoteQsoCount = remoteCount,
            RemoteOwner = string.IsNullOrWhiteSpace(remoteOwner) ? null : remoteOwner,
            ErrorSummary = errors.Count > 0 ? string.Join("; ", errors) : null,
            RemoteDeletesPushed = remoteDeletesPushed,
            DeletesSkippedRemote = deletesSkippedRemote,
            DuplicateReplaceCount = duplicateReplaces,
        };
    }

    private sealed record UploadPassResult(
        uint Uploaded,
        uint DuplicateReplaces,
        HashSet<string> UploadedLogids);

    private async Task<UploadPassResult> UploadPendingQsosAsync(
        ILogbookStore store,
        string? bookOwner,
        Func<QsoRecord, bool> shouldUpload,
        List<string> errors)
    {
        IReadOnlyList<QsoRecord> pendingQsos;
        try
        {
            var allQsos = await store.ListQsosAsync(new QsoListQuery { Sort = QsoSortOrder.OldestFirst }).ConfigureAwait(false);
            pendingQsos = allQsos.Where(shouldUpload).ToList();
        }
        catch (Exception ex)
        {
            errors.Add($"Failed to list pending QSOs for upload: {ex.Message}");
            pendingQsos = [];
        }

        uint uploaded = 0;
        uint duplicateReplaces = 0;
        var uploadedLogids = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        foreach (var qso in pendingQsos)
        {
            try
            {
                string logid;
                var hasExistingLogid = !string.IsNullOrWhiteSpace(qso.QrzLogid);

                if (qso.SyncStatus == SyncStatus.Modified && hasExistingLogid)
                {
                    logid = await _client.UpdateQsoAsync(qso, bookOwner).ConfigureAwait(false);
                }
                else
                {
                    try
                    {
                        logid = await _client.UploadQsoAsync(qso, bookOwner).ConfigureAwait(false);
                    }
                    catch (QrzLogbookException ex) when (!hasExistingLogid && IsDuplicateError(ex.Message))
                    {
                        // QSO already exists on QRZ (e.g. uploaded via web UI) but we
                        // don't have the logid locally. Retry with OPTION=REPLACE to
                        // auto-match and adopt the remote logid.
                        logid = await _client.UploadQsoWithReplaceAsync(qso, bookOwner).ConfigureAwait(false);
                        duplicateReplaces++;
                    }
                }

                var synced = qso.Clone();
                synced.QrzLogid = logid;
                synced.SyncStatus = SyncStatus.Synced;
                try
                {
                    await store.UpdateQsoAsync(synced).ConfigureAwait(false);
                    uploadedLogids.Add(logid);
                }
                catch (Exception ex)
                {
                    errors.Add($"Upload succeeded for {qso.WorkedCallsign} but local update failed: {ex.Message}");
                }

                uploaded++;
            }
            catch (Exception ex)
            {
                errors.Add($"Upload failed for {qso.WorkedCallsign}: {ex.Message}");
            }
        }

        return new UploadPassResult(uploaded, duplicateReplaces, uploadedLogids);
    }

    // -- Matching helpers ---------------------------------------------------

    private static (Dictionary<string, QsoRecord> ByLogid, Dictionary<(string Call, Band Band, Mode Mode), List<QsoRecord>> ByKey)
        BuildLocalIndexes(IReadOnlyList<QsoRecord> localQsos)
    {
        var byLogid = new Dictionary<string, QsoRecord>(StringComparer.OrdinalIgnoreCase);
        var byKey = new Dictionary<(string, Band, Mode), List<QsoRecord>>();

        foreach (var qso in localQsos)
        {
            var logid = ExtractQrzLogid(qso);
            if (logid is not null)
            {
                byLogid.TryAdd(logid, qso);
            }

            var key = MakeFuzzyKey(qso);
            if (!byKey.TryGetValue(key, out var list))
            {
                list = [];
                byKey[key] = list;
            }

            list.Add(qso);
        }

        return (byLogid, byKey);
    }

    private static (string Call, Band Band, Mode Mode) MakeFuzzyKey(QsoRecord qso) =>
        (qso.WorkedCallsign.ToUpperInvariant(), qso.Band, qso.Mode);

    private static QsoRecord? FuzzyMatch(QsoRecord remote, Dictionary<(string, Band, Mode), List<QsoRecord>> byKey)
    {
        var key = MakeFuzzyKey(remote);
        if (!byKey.TryGetValue(key, out var candidates))
        {
            return null;
        }

        var remoteTs = remote.UtcTimestamp?.Seconds ?? 0;
        return candidates.Find(local =>
            local.UtcTimestamp is not null
            && Math.Abs(local.UtcTimestamp.Seconds - remoteTs) <= TimestampToleranceSeconds);
    }

    /// <summary>
    /// Extract the QRZ logbook record ID from a QSO, checking the dedicated field first,
    /// then falling back to common extra-field keys.
    /// </summary>
    internal static string? ExtractQrzLogid(QsoRecord qso)
    {
        if (qso.HasQrzLogid && !string.IsNullOrWhiteSpace(qso.QrzLogid))
        {
            return qso.QrzLogid;
        }

        if (qso.ExtraFields.TryGetValue(QrzLogidAltExtraField, out var alt) && !string.IsNullOrWhiteSpace(alt))
        {
            return alt;
        }

        if (qso.ExtraFields.TryGetValue(QrzLogidExtraField, out var logid) && !string.IsNullOrWhiteSpace(logid))
        {
            return logid;
        }

        return null;
    }

    /// <summary>
    /// Merge remote QSO data into an existing local QSO, honoring the requested conflict policy.
    /// Returns the merged record along with a flag indicating whether this merge required
    /// operator attention (conflict) — which is only true when the policy is FlagForReview
    /// and the local row had unsynced edits.
    /// </summary>
    private static (QsoRecord Merged, bool IsConflict) MergeRemoteIntoLocal(
        QsoRecord local,
        QsoRecord remote,
        string? remoteLogid,
        ConflictPolicy policy)
    {
        var localHasUnsyncedEdits = local.SyncStatus == SyncStatus.Modified;

        // Happy path: local was already Synced (never edited since last sync) or the
        // QSO is LocalOnly with no prior remote link. Remote wins for previously-synced
        // rows (they reflect authoritative QRZ state). For LocalOnly rows, keep the
        // locally logged contest/contact data but import QRZ's enrichment into blanks.
        if (!localHasUnsyncedEdits)
        {
            var nonConflict = local.SyncStatus == SyncStatus.Synced ? remote.Clone() : local.Clone();
            if (local.SyncStatus == SyncStatus.LocalOnly)
            {
                FillMissingRemoteEnrichment(nonConflict, remote);
            }

            nonConflict.LocalId = local.LocalId;
            nonConflict.SyncStatus = SyncStatus.Synced;
            nonConflict.QrzLogid = remoteLogid ?? local.QrzLogid;
            return (nonConflict, false);
        }

        // Conflict: local edits exist and remote also has a current version.
        // The resolver choice determines who wins and whether we mark the row
        // for operator review per engine spec §6.3.
        QsoRecord merged;
        bool requiresReview;
        switch (policy)
        {
            case ConflictPolicy.LastWriteWins:
                // Remote wins silently — no operator intervention needed, so
                // this is NOT counted as a conflict in the sync result.
                merged = remote.Clone();
                merged.LocalId = local.LocalId;
                merged.SyncStatus = SyncStatus.Synced;
                requiresReview = false;
                break;

            case ConflictPolicy.FlagForReview:
            default:
                // Preserve the local edit but mark the row so operators can
                // reconcile manually. Do NOT silently discard user data.
                merged = local.Clone();
                merged.LocalId = local.LocalId;
                merged.SyncStatus = SyncStatus.Conflict;
                requiresReview = true;
                break;
        }

        merged.QrzLogid = remoteLogid ?? local.QrzLogid;
        return (merged, requiresReview);
    }

    private static void FillMissingRemoteEnrichment(QsoRecord target, QsoRecord remote)
    {
        CopyStringIfMissing(
            target.HasWorkedOperatorCallsign,
            target.WorkedOperatorCallsign,
            remote.HasWorkedOperatorCallsign,
            remote.WorkedOperatorCallsign,
            value => target.WorkedOperatorCallsign = value);
        CopyStringIfMissing(
            target.HasWorkedOperatorName,
            target.WorkedOperatorName,
            remote.HasWorkedOperatorName,
            remote.WorkedOperatorName,
            value => target.WorkedOperatorName = value);
        CopyStringIfMissing(
            target.HasWorkedGrid,
            target.WorkedGrid,
            remote.HasWorkedGrid,
            remote.WorkedGrid,
            value => target.WorkedGrid = value);
        CopyStringIfMissing(
            target.HasWorkedCountry,
            target.WorkedCountry,
            remote.HasWorkedCountry,
            remote.WorkedCountry,
            value => target.WorkedCountry = value);
        CopyUInt32IfMissing(
            target.HasWorkedDxcc,
            target.WorkedDxcc,
            remote.HasWorkedDxcc,
            remote.WorkedDxcc,
            value => target.WorkedDxcc = value);
        CopyStringIfMissing(
            target.HasWorkedState,
            target.WorkedState,
            remote.HasWorkedState,
            remote.WorkedState,
            value => target.WorkedState = value);
        CopyUInt32IfMissing(
            target.HasWorkedCqZone,
            target.WorkedCqZone,
            remote.HasWorkedCqZone,
            remote.WorkedCqZone,
            value => target.WorkedCqZone = value);
        CopyUInt32IfMissing(
            target.HasWorkedItuZone,
            target.WorkedItuZone,
            remote.HasWorkedItuZone,
            remote.WorkedItuZone,
            value => target.WorkedItuZone = value);
        CopyStringIfMissing(
            target.HasWorkedCounty,
            target.WorkedCounty,
            remote.HasWorkedCounty,
            remote.WorkedCounty,
            value => target.WorkedCounty = value);
        CopyStringIfMissing(
            target.HasWorkedIota,
            target.WorkedIota,
            remote.HasWorkedIota,
            remote.WorkedIota,
            value => target.WorkedIota = value);
        CopyStringIfMissing(
            target.HasWorkedContinent,
            target.WorkedContinent,
            remote.HasWorkedContinent,
            remote.WorkedContinent,
            value => target.WorkedContinent = value);
        CopyStringIfMissing(
            target.HasWorkedArrlSection,
            target.WorkedArrlSection,
            remote.HasWorkedArrlSection,
            remote.WorkedArrlSection,
            value => target.WorkedArrlSection = value);
        CopyStringIfMissing(
            target.HasSkcc,
            target.Skcc,
            remote.HasSkcc,
            remote.Skcc,
            value => target.Skcc = value);
        CopyDoubleIfMissing(
            target.HasWorkedLatitude,
            target.WorkedLatitude,
            remote.HasWorkedLatitude,
            remote.WorkedLatitude,
            value => target.WorkedLatitude = value);
        CopyDoubleIfMissing(
            target.HasWorkedLongitude,
            target.WorkedLongitude,
            remote.HasWorkedLongitude,
            remote.WorkedLongitude,
            value => target.WorkedLongitude = value);
        CopyDoubleIfMissing(
            target.HasWorkedAltitudeMeters,
            target.WorkedAltitudeMeters,
            remote.HasWorkedAltitudeMeters,
            remote.WorkedAltitudeMeters,
            value => target.WorkedAltitudeMeters = value);
        CopyStringIfMissing(
            target.HasWorkedGridsquareExt,
            target.WorkedGridsquareExt,
            remote.HasWorkedGridsquareExt,
            remote.WorkedGridsquareExt,
            value => target.WorkedGridsquareExt = value);

        foreach (var pair in remote.ExtraFields)
        {
            if (!string.IsNullOrWhiteSpace(pair.Value)
                && (!target.ExtraFields.TryGetValue(pair.Key, out var existing)
                    || string.IsNullOrWhiteSpace(existing)))
            {
                target.ExtraFields[pair.Key] = pair.Value;
            }
        }
    }

    private static void CopyStringIfMissing(
        bool targetHasValue,
        string targetValue,
        bool remoteHasValue,
        string remoteValue,
        Action<string> setter)
    {
        if ((!targetHasValue || string.IsNullOrWhiteSpace(targetValue))
            && remoteHasValue
            && !string.IsNullOrWhiteSpace(remoteValue))
        {
            setter(remoteValue.Trim());
        }
    }

    private static void CopyUInt32IfMissing(
        bool targetHasValue,
        uint targetValue,
        bool remoteHasValue,
        uint remoteValue,
        Action<uint> setter)
    {
        if ((!targetHasValue || targetValue == 0) && remoteHasValue && remoteValue > 0)
        {
            setter(remoteValue);
        }
    }

    private static void CopyDoubleIfMissing(
        bool targetHasValue,
        double targetValue,
        bool remoteHasValue,
        double remoteValue,
        Action<double> setter)
    {
        if ((!targetHasValue || !double.IsFinite(targetValue))
            && remoteHasValue
            && double.IsFinite(remoteValue))
        {
            setter(remoteValue);
        }
    }

    private static string? FormatSinceDate(SyncMetadata metadata)
    {
        if (metadata.LastSync is not { } lastSync)
        {
            return null;
        }

        return lastSync.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture);
    }

    /// <summary>
    /// Check whether a QRZ API error message indicates a duplicate QSO.
    /// </summary>
    private static bool IsDuplicateError(string message) =>
        message.Contains("duplicate", StringComparison.OrdinalIgnoreCase);
}

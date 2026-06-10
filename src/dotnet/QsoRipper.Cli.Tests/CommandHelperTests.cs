using QsoRipper.Cli.Commands;
using QsoRipper.Domain;
using QsoRipper.Services;

namespace QsoRipper.Cli.Tests;

#pragma warning disable CA1707 // Remove underscores from member names - xUnit allows underscores in test methods
[Collection("ConsoleCapture")]
public sealed class CommandHelperTests
{
    [Fact]
    public void TryBuildQso_populates_expected_fields()
    {
        var success = LogQsoCommand.TryBuildQso(
            "W1AW",
            ["20m", "FT8", "--station", "k7abv", "--rst-sent", "59", "--rst-rcvd", "57", "--freq", "14.074", "--comment", "Strong copy", "--notes", "Worked on dipole"],
            out var qso,
            out _,
            out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.NotNull(qso);
        Assert.Equal("W1AW", qso!.WorkedCallsign);
        Assert.Equal("K7ABV", qso.StationCallsign);
        Assert.Equal(Band._20M, qso.Band);
        Assert.Equal(Mode.Ft8, qso.Mode);
        Assert.Equal((ulong)14_074_000, qso.FrequencyHz);
        Assert.Equal((uint)5, qso.RstSent!.Readability);
        Assert.Equal((uint)9, qso.RstSent.Strength);
        Assert.Equal((uint)5, qso.RstReceived!.Readability);
        Assert.Equal((uint)7, qso.RstReceived.Strength);
        Assert.NotNull(qso.UtcTimestamp);
        Assert.Equal("Strong copy", qso.Comment);
        Assert.Equal("Worked on dipole", qso.Notes);
    }

    [Theory]
    [InlineData("--freq", "nope", "Invalid value for --freq: nope. Use MHz such as 14.074.")]
    [InlineData("--rst-sent", "ab", "Invalid value for --rst-sent: ab. Expected 2 or 3 digits.")]
    [InlineData("--rst-rcvd", "1", "Invalid value for --rst-rcvd: 1. Expected 2 or 3 digits.")]
    public void TryBuildQso_rejects_invalid_optional_values(string option, string value, string expectedError)
    {
        var success = LogQsoCommand.TryBuildQso(
            "W1AW",
            ["20m", "FT8", option, value],
            out _,
            out _,
            out var error);

        Assert.False(success);
        Assert.Equal(expectedError, error);
    }

    [Theory]
    [InlineData("--station", "Missing value for --station.")]
    [InlineData("--freq", "Missing value for --freq.")]
    [InlineData("--comment", "Missing value for --comment.")]
    [InlineData("--notes", "Missing value for --notes.")]
    public void TryBuildQso_rejects_missing_option_values(string option, string expectedError)
    {
        var success = LogQsoCommand.TryBuildQso("W1AW", ["20m", "FT8", option], out _, out _, out var error);

        Assert.False(success);
        Assert.Equal(expectedError, error);
    }

    [Fact]
    public void TryParseRst_accepts_three_digits()
    {
        var success = LogQsoCommand.TryParseRst("599", out var report);

        Assert.True(success);
        Assert.Equal((uint)5, report.Readability);
        Assert.Equal((uint)9, report.Strength);
        Assert.Equal((uint)9, report.Tone);
    }

    [Theory]
    [InlineData("")]
    [InlineData("5")]
    [InlineData("abcd")]
    public void TryParseRst_rejects_invalid_values(string value)
    {
        Assert.False(LogQsoCommand.TryParseRst(value, out _));
    }

    [Theory]
    [InlineData(Mode.Ssb, 5u, 9u, 0u)]
    [InlineData(Mode.Am, 5u, 9u, 0u)]
    [InlineData(Mode.Fm, 5u, 9u, 0u)]
    [InlineData(Mode.Cw, 5u, 9u, 9u)]
    [InlineData(Mode.Ft8, 5u, 9u, 9u)]
    [InlineData(Mode.Rtty, 5u, 9u, 9u)]
    public void DefaultRst_returns_59_for_phone_and_599_for_digital(Mode mode, uint readability, uint strength, uint tone)
    {
        var rst = LogQsoCommand.DefaultRst(mode);

        Assert.Equal(readability, rst.Readability);
        Assert.Equal(strength, rst.Strength);
        Assert.Equal(tone, rst.Tone);
    }

    [Fact]
    public void ApplyDefaultRst_does_not_overwrite_explicit_values()
    {
        var qso = new QsoRecord
        {
            Mode = Mode.Cw,
            RstSent = new RstReport { Readability = 4, Strength = 7, Tone = 8 },
        };

        LogQsoCommand.ApplyDefaultRst(qso);

        Assert.Equal(4u, qso.RstSent.Readability);
        Assert.Equal(7u, qso.RstSent.Strength);
        Assert.Equal(8u, qso.RstSent.Tone);
        Assert.Equal(5u, qso.RstReceived!.Readability);
        Assert.Equal(9u, qso.RstReceived.Strength);
        Assert.Equal(9u, qso.RstReceived.Tone);
    }

    [Fact]
    public void TryBuildQso_without_rst_flags_gets_defaults_after_apply()
    {
        var success = LogQsoCommand.TryBuildQso("W1AW", ["20m", "SSB"], out var qso, out _, out _);

        Assert.True(success);
        Assert.Null(qso!.RstSent);

        LogQsoCommand.ApplyDefaultRst(qso);

        Assert.Equal(5u, qso.RstSent!.Readability);
        Assert.Equal(9u, qso.RstSent.Strength);
        Assert.Equal(0u, qso.RstSent.Tone);
        Assert.Equal(5u, qso.RstReceived!.Readability);
        Assert.Equal(9u, qso.RstReceived.Strength);
        Assert.Equal(0u, qso.RstReceived.Tone);
    }

    [Fact]
    public void TryBuildQso_from_rig_allows_omitting_band_mode()
    {
        var success = LogQsoCommand.TryBuildQso(
            "W1AW",
            ["--from-rig"],
            out var qso,
            out _,
            out var fromRig,
            out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.True(fromRig);
        Assert.NotNull(qso);
        Assert.Equal(Band.Unspecified, qso!.Band);
        Assert.Equal(Mode.Unspecified, qso.Mode);
    }

    [Fact]
    public void TryBuildQso_from_rig_with_explicit_band_mode()
    {
        var success = LogQsoCommand.TryBuildQso(
            "W1AW",
            ["40m", "CW", "--from-rig"],
            out var qso,
            out _,
            out var fromRig,
            out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.True(fromRig);
        Assert.NotNull(qso);
        Assert.Equal(Band._40M, qso!.Band);
        Assert.Equal(Mode.Cw, qso.Mode);
    }

    [Fact]
    public void TryBuildQso_from_rig_with_named_band_mode_overrides()
    {
        var success = LogQsoCommand.TryBuildQso(
            "W1AW",
            ["--from-rig", "--band", "20m", "--mode", "FT8"],
            out var qso,
            out _,
            out var fromRig,
            out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.True(fromRig);
        Assert.NotNull(qso);
        Assert.Equal(Band._20M, qso!.Band);
        Assert.Equal(Mode.Ft8, qso.Mode);
    }

    [Fact]
    public void TryBuildQso_without_from_rig_requires_band_mode()
    {
        var success = LogQsoCommand.TryBuildQso(
            "W1AW",
            [],
            out _,
            out _,
            out _);

        Assert.False(success);
    }

    [Fact]
    public void TryParseArgs_populates_filters()
    {
        var success = ListQsosCommand.TryParseArgs(["--callsign", "w1aw", "--band", "20m", "--mode", "ft8", "--after", "2026-04-10T00:00:00Z", "--before", "2026-04-11T00:00:00Z", "--limit", "5"],
            out var request,
            out var displayOptions,
            out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.Equal("W1AW", request.CallsignFilter);
        Assert.Equal(Band._20M, request.BandFilter);
        Assert.Equal(Mode.Ft8, request.ModeFilter);
        Assert.Equal((uint)5, request.Limit);
        Assert.NotNull(request.After);
        Assert.NotNull(request.Before);
        Assert.True(displayOptions.ShowComment);
    }

    [Fact]
    public void FormatCommentPreview_prefers_comment_then_trims()
    {
        var qso = new QsoRecord
        {
            Comment = "This is a very long comment that should be trimmed before it reaches the default list output column.",
            Notes = "backup"
        };

        var preview = ListQsosCommand.FormatCommentPreview(qso);

        Assert.Equal("This is a very long comment that shou...", preview);
    }

    [Fact]
    public void FormatCommentPreview_falls_back_to_notes_and_flattens_newlines()
    {
        var qso = new QsoRecord
        {
            Notes = "first line" + Environment.NewLine + "second line"
        };

        var preview = ListQsosCommand.FormatCommentPreview(qso);

        Assert.Equal("first line second line", preview);
    }

    public static TheoryData<string[], string> InvalidListArgs { get; } =
        new()
        {
            { ["--limit", "oops"], "Invalid value for --limit: oops" },
            { ["--after", "yesterdayish"], "Invalid --after value. Use relative (2.days, 3.hours) or absolute (2026-04-10)." },
            { ["--after", "2147483647.months"], "Invalid --after value. Use relative (2.days, 3.hours) or absolute (2026-04-10)." },
            { ["--callsign"], "Missing value for --callsign." },
            { ["--unknown"], "Unknown option: --unknown" }
        };

    [Theory]
    [MemberData(nameof(InvalidListArgs))]
    public void TryParseArgs_rejects_invalid_args(string[] args, string expectedError)
    {
        var success = ListQsosCommand.TryParseArgs(args, out _, out _, out var error);

        Assert.False(success);
        Assert.Equal(expectedError, error);
    }

    public static TheoryData<RstReport?, string> FormattedRstValues { get; } =
        new()
        {
            { null, "" },
            { new RstReport { Readability = 5, Strength = 9 }, "59" },
            { new RstReport { Readability = 5, Strength = 9, Tone = 9 }, "599" },
            { new RstReport { Raw = "599" }, "599" },
        };

    [Theory]
    [MemberData(nameof(FormattedRstValues))]
    public void FormatRst_formats_reports(RstReport? rst, string expected)
    {
        Assert.Equal(expected, ListQsosCommand.FormatRst(rst));
    }

    [Fact]
    public void TryApplyUpdates_sets_grid_and_freq()
    {
        var qso = new QsoRecord { WorkedCallsign = "W1AW", Band = Band._20M, Mode = Mode.Cw };
        var enrich = false;

        var success = UpdateQsoCommand.TryApplyUpdates(
            ["--grid", "FN31", "--freq", "14.035", "--comment", "Nice signal"],
            qso,
            ref enrich,
            out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.Equal("FN31", qso.WorkedGrid);
        Assert.Equal((ulong)14_035_000, qso.FrequencyHz);
        Assert.Equal("Nice signal", qso.Comment);
        Assert.False(enrich);
    }

    [Fact]
    public void TryApplyUpdates_sets_enrich_flag()
    {
        var qso = new QsoRecord { WorkedCallsign = "W1AW" };
        var enrich = false;

        var success = UpdateQsoCommand.TryApplyUpdates(["--enrich"], qso, ref enrich, out _);

        Assert.True(success);
        Assert.True(enrich);
    }

    [Fact]
    public void TryApplyUpdates_sets_country_state_band_mode()
    {
        var qso = new QsoRecord { WorkedCallsign = "W1AW", Band = Band._20M, Mode = Mode.Cw };
        var enrich = false;

        var success = UpdateQsoCommand.TryApplyUpdates(
            ["--country", "United States", "--state", "ct", "--band", "40m", "--mode", "SSB"],
            qso,
            ref enrich,
            out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.Equal("United States", qso.WorkedCountry);
        Assert.Equal("CT", qso.WorkedState);
        Assert.Equal(Band._40M, qso.Band);
        Assert.Equal(Mode.Ssb, qso.Mode);
    }

    [Fact]
    public void TryApplyUpdates_sets_rst_sent_and_received()
    {
        var qso = new QsoRecord { WorkedCallsign = "W1AW", Band = Band._20M, Mode = Mode.Cw };
        var enrich = false;

        var success = UpdateQsoCommand.TryApplyUpdates(
            ["--rst-sent", "579", "--rst-rcvd", "589"],
            qso,
            ref enrich,
            out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.Equal(5u, qso.RstSent!.Readability);
        Assert.Equal(7u, qso.RstSent.Strength);
        Assert.Equal(9u, qso.RstSent.Tone);
        Assert.Equal(5u, qso.RstReceived!.Readability);
        Assert.Equal(8u, qso.RstReceived.Strength);
        Assert.Equal(9u, qso.RstReceived.Tone);
    }

    [Fact]
    public void TryApplyUpdates_sets_timestamp_with_at_flag()
    {
        var qso = new QsoRecord { WorkedCallsign = "W1AW", Band = Band._20M, Mode = Mode.Cw };
        var enrich = false;

        var success = UpdateQsoCommand.TryApplyUpdates(
            ["--at", "2026-04-12T01:51:00Z"],
            qso,
            ref enrich,
            out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.NotNull(qso.UtcTimestamp);
        Assert.Equal(2026, qso.UtcTimestamp.ToDateTime().Year);
        Assert.Equal(4, qso.UtcTimestamp.ToDateTime().Month);
        Assert.Equal(12, qso.UtcTimestamp.ToDateTime().Day);
        Assert.Equal(1, qso.UtcTimestamp.ToDateTime().Hour);
        Assert.Equal(51, qso.UtcTimestamp.ToDateTime().Minute);
    }

    [Fact]
    public void TryApplyUpdates_rejects_invalid_at_value()
    {
        var qso = new QsoRecord { WorkedCallsign = "W1AW" };
        var enrich = false;

        var success = UpdateQsoCommand.TryApplyUpdates(["--at", "not-a-time"], qso, ref enrich, out var error);

        Assert.False(success);
        Assert.Contains("Invalid --at value", error, StringComparison.Ordinal);
    }

    [Fact]
    public void TryApplyUpdates_rejects_oversized_relative_at_value()
    {
        var qso = new QsoRecord { WorkedCallsign = "W1AW" };
        var enrich = false;

        var success = UpdateQsoCommand.TryApplyUpdates(["--at", "2147483647.months"], qso, ref enrich, out var error);

        Assert.False(success);
        Assert.Contains("Invalid --at value", error, StringComparison.Ordinal);
    }

    [Theory]
    [InlineData("--at", "Missing value for --at.")]
    [InlineData("--grid", "Missing value for --grid.")]
    [InlineData("--country", "Missing value for --country.")]
    [InlineData("--state", "Missing value for --state.")]
    [InlineData("--freq", "Missing value for --freq.")]
    [InlineData("--band", "Missing value for --band.")]
    [InlineData("--mode", "Missing value for --mode.")]
    [InlineData("--comment", "Missing value for --comment.")]
    [InlineData("--rst-sent", "Missing value for --rst-sent.")]
    [InlineData("--rst-rcvd", "Missing value for --rst-rcvd.")]
    public void TryApplyUpdates_rejects_missing_values(string option, string expectedError)
    {
        var qso = new QsoRecord { WorkedCallsign = "W1AW" };
        var enrich = false;

        var success = UpdateQsoCommand.TryApplyUpdates([option], qso, ref enrich, out var error);

        Assert.False(success);
        Assert.Equal(expectedError, error);
    }

    [Fact]
    public void TryApplyUpdates_rejects_unknown_option()
    {
        var qso = new QsoRecord { WorkedCallsign = "W1AW" };
        var enrich = false;

        var success = UpdateQsoCommand.TryApplyUpdates(["--bogus"], qso, ref enrich, out var error);

        Assert.False(success);
        Assert.Equal("Unknown option: --bogus", error);
    }

    [Fact]
    public async Task ReadRequestsAsync_splits_stream_into_chunks()
    {
        var bytes = Enumerable.Range(0, 131077).Select(static i => (byte)(i % 251)).ToArray();
        using var stream = new MemoryStream(bytes);

        var requests = new List<ImportAdifRequest>();
        await foreach (var request in ImportAdifCommand.ReadRequestsAsync(stream))
        {
            requests.Add(request);
        }

        Assert.Equal(3, requests.Count);
        Assert.Equal(65536, requests[0].Chunk!.Data.Length);
        Assert.Equal(65536, requests[1].Chunk!.Data.Length);
        Assert.Equal(5, requests[2].Chunk!.Data.Length);

        var reconstructed = requests.SelectMany(static request => request.Chunk!.Data.ToByteArray()).ToArray();
        Assert.Equal(bytes, reconstructed);
    }

    [Fact]
    public void ResolveOutputFile_generates_dated_filename_for_existing_directory()
    {
        var outputDirectory = Directory.CreateTempSubdirectory();
        try
        {
            var resolved = ExportAdifCommand.ResolveOutputFile(
                outputDirectory.FullName,
                "C:\\logs\\kc7ava-debug-log.db",
                new DateOnly(2026, 5, 7));

            Assert.Equal(
                Path.Combine(outputDirectory.FullName, "kc7ava-debug-log-2026-05-07.adi"),
                resolved);
        }
        finally
        {
            outputDirectory.Delete(recursive: true);
        }
    }

    [Fact]
    public void ResolveOutputFile_treats_trailing_separator_as_directory_and_avoids_overwrite()
    {
        var root = Directory.CreateTempSubdirectory();
        try
        {
            var outputDirectory = Path.Combine(root.FullName, "Backups") + Path.DirectorySeparatorChar;
            Directory.CreateDirectory(outputDirectory);
            File.WriteAllText(Path.Combine(outputDirectory, "qsoripper-logbook-2026-05-07.adi"), "existing");

            var resolved = ExportAdifCommand.ResolveOutputFile(
                outputDirectory,
                null,
                new DateOnly(2026, 5, 7));

            Assert.Equal(
                Path.Combine(outputDirectory, "qsoripper-logbook-2026-05-07-001.adi"),
                resolved);
        }
        finally
        {
            root.Delete(recursive: true);
        }
    }

    [Fact]
    public void ResolveOutputFile_keeps_exact_file_paths_exact()
    {
        var exactPath = Path.Combine(Path.GetTempPath(), "my-log.adi");

        var resolved = ExportAdifCommand.ResolveOutputFile(
            exactPath,
            "C:\\logs\\kc7ava-debug-log.db",
            new DateOnly(2026, 5, 7));

        Assert.Equal(exactPath, resolved);
    }

    [Fact]
    public async Task Export_returns_error_when_exact_file_parent_is_missing()
    {
        var originalError = Console.Error;
        using var error = new StringWriter();
        Console.SetError(error);
        try
        {
            var missingParentFile = Path.Combine(
                Path.GetTempPath(),
                Guid.NewGuid().ToString("N"),
                "backup.adi");

            var exitCode = await ExportAdifCommand.RunAsync(null!, ["--file", missingParentFile]);

            Assert.Equal(1, exitCode);
            Assert.Contains("Unable to open ADIF export file", error.ToString(), StringComparison.Ordinal);
        }
        finally
        {
            Console.SetError(originalError);
        }
    }

    [Fact]
    public void Export_TryParseArgs_accepts_after_before_and_contest_filters()
    {
        var request = new ExportAdifRequest();
        var success = ExportAdifCommand.TryParseArgs(
            ["--after", "2026-04-21T18:03:42Z", "--before", "2026-05-01", "--contest", "CQWWSSB", "--include-header"],
            request,
            out var outputFile,
            out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.Null(outputFile);
        Assert.True(request.IncludeHeader);
        Assert.Equal("CQWWSSB", request.ContestId);
        Assert.NotNull(request.After);
        Assert.Equal(new DateTime(2026, 4, 21, 18, 3, 42, DateTimeKind.Utc), request.After!.ToDateTime());
        Assert.NotNull(request.Before);
        Assert.Equal(new DateTime(2026, 5, 1, 0, 0, 0, DateTimeKind.Utc), request.Before!.ToDateTime());
    }

    [Fact]
    public void Export_TryParseArgs_accepts_relative_after_filter()
    {
        var request = new ExportAdifRequest();
        var before = DateTime.UtcNow;
        var success = ExportAdifCommand.TryParseArgs(
            ["--after", "2.days"],
            request,
            out _,
            out var error);
        var after = DateTime.UtcNow;

        Assert.True(success);
        Assert.Null(error);
        Assert.NotNull(request.After);
        var parsed = request.After!.ToDateTime();
        Assert.InRange(parsed, before.AddDays(-2).AddSeconds(-5), after.AddDays(-2).AddSeconds(5));
    }

    [Fact]
    public void Export_TryParseArgs_rejects_invalid_after_value()
    {
        var request = new ExportAdifRequest();
        var success = ExportAdifCommand.TryParseArgs(
            ["--after", "not-a-time"],
            request,
            out _,
            out var error);

        Assert.False(success);
        Assert.NotNull(error);
        Assert.Contains("--after", error, StringComparison.Ordinal);
    }

    [Fact]
    public void Export_TryParseArgs_rejects_missing_value_for_filters()
    {
        var request = new ExportAdifRequest();
        var success = ExportAdifCommand.TryParseArgs(
            ["--before"],
            request,
            out _,
            out var error);

        Assert.False(success);
        Assert.Equal("Missing value for --before.", error);
    }

    [Fact]
    public void Export_TryParseArgs_rejects_unknown_option()
    {
        var request = new ExportAdifRequest();
        var success = ExportAdifCommand.TryParseArgs(
            ["--bogus"],
            request,
            out _,
            out var error);

        Assert.False(success);
        Assert.NotNull(error);
        Assert.Contains("--bogus", error, StringComparison.Ordinal);
    }

    [Fact]
    public void TryParseArgs_deleted_flag_sets_deleted_only_filter()
    {
        var success = ListQsosCommand.TryParseArgs(["--deleted"], out var request, out var displayOptions, out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.Equal(DeletedRecordsFilter.DeletedOnly, request.DeletedFilter);
        Assert.True(displayOptions.ShowDeletedAt);
    }

    [Fact]
    public void TryParseArgs_include_deleted_flag_sets_all_filter()
    {
        var success = ListQsosCommand.TryParseArgs(["--include-deleted"], out var request, out var displayOptions, out var error);

        Assert.True(success);
        Assert.Null(error);
        Assert.Equal(DeletedRecordsFilter.All, request.DeletedFilter);
        Assert.True(displayOptions.ShowDeletedAt);
    }

    [Fact]
    public void TryParseArgs_combining_deleted_and_include_deleted_is_rejected()
    {
        var success = ListQsosCommand.TryParseArgs(["--deleted", "--include-deleted"], out _, out _, out var error);

        Assert.False(success);
        Assert.NotNull(error);
        Assert.Contains("--deleted", error, StringComparison.Ordinal);
        Assert.Contains("--include-deleted", error, StringComparison.Ordinal);
    }

    [Fact]
    public void TryParseArgs_no_delete_flag_uses_default_filter()
    {
        var success = ListQsosCommand.TryParseArgs([], out var request, out var displayOptions, out _);

        Assert.True(success);
        Assert.Equal(DeletedRecordsFilter.Unspecified, request.DeletedFilter);
        Assert.False(displayOptions.ShowDeletedAt);
    }
}
#pragma warning restore CA1707

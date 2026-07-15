namespace QsoRipper.Engine.DotNet;

/// <summary>Runs configured QRZ synchronization without blocking local logging.</summary>
internal sealed class QrzSyncSupervisor(ManagedEngineState state) : BackgroundService
{
    private static readonly TimeSpan PollInterval = TimeSpan.FromSeconds(1);

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        DateTimeOffset? dueAt = null;

        while (!stoppingToken.IsCancellationRequested)
        {
            var settings = state.GetAutomaticSyncSettings();
            if (!settings.Enabled)
            {
                dueAt = null;
                state.SetNextAutomaticSync(null);
            }
            else
            {
                dueAt ??= DateTimeOffset.UtcNow.Add(settings.Interval);
                state.SetNextAutomaticSync(dueAt);

                if (DateTimeOffset.UtcNow >= dueAt)
                {
                    try
                    {
                        await Task.Run(() => state.SyncWithQrz(), stoppingToken).ConfigureAwait(false);
                    }
                    catch (QrzSyncUnavailableException)
                    {
                        // Configuration can change between the schedule check
                        // and execution. The next poll recomputes the schedule.
                    }

                    dueAt = DateTimeOffset.UtcNow.Add(settings.Interval);
                    state.SetNextAutomaticSync(dueAt);
                }
            }

            await Task.Delay(PollInterval, stoppingToken).ConfigureAwait(false);
        }
    }
}

/**
 * Bind an ephemeral inspector stream to one Session.
 * @param remote - Generated Remote transport and reconnecting stream owner.
 * @param sessionId - Session inspected by this view.
 * @param report - Diagnostic sink for unavailable transport and teardown failures.
 * @returns Subscription factory; closing a subscription releases its selected value.
 */
export function createToolResultWatcher(remote, sessionId, report) {
    return (seq, publish) => {
        const lifetime = { stopped: false };
        const clear = () => { if (!lifetime.stopped)
            publish(undefined); };
        const stream = remote.$stream({
            name: 'live tool result',
            open: signal => remote.toolResults.watch({ sessionId, seq }, signal),
            ended: () => new Error('live tool result stream ended'),
            carrierFailed: clear,
        });
        void (async () => {
            try {
                for await (const frame of stream) {
                    if (lifetime.stopped)
                        return;
                    frame.accept();
                    publish(frame.value.available ? frame.value.value : undefined);
                }
            }
            catch (error) {
                if (!lifetime.stopped) {
                    clear();
                    report(error);
                }
            }
            finally {
                clear();
            }
        })();
        return () => {
            lifetime.stopped = true;
            void stream.dispose().catch(report);
        };
    };
}
//# sourceMappingURL=watch-tool-result.js.map
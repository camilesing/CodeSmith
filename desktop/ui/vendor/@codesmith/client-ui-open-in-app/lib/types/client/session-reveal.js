/**
 * Share directory availability and launch feedback with the existing application controls.
 * @param ctx - owning plugin context.
 * @param controller - directory application catalog and launcher.
 * @param t - localized action and failure labels.
 * @param report - native operation feedback shared with the application controls.
 */
export function registerSessionReveal(ctx, controller, t, report) {
    ctx.effect(() => ctx.contextMenus.register({
        id: 'open-in-app.reveal', target: 'session', order: 500,
        observe: () => [controller.apps, ctx.sessions.list, ctx.locale],
        resolve: ({ sessionId }) => {
            const directory = ctx.sessions.list.getSnapshot().byId[sessionId]?.cwd;
            const applications = controller.apps.getSnapshot();
            const manager = applications?.find(id => ['finder', 'explorer', 'filemanager'].includes(id));
            return [{
                    id: 'reveal', label: t(applications?.includes('finder') ? 'menu.finder' : 'menu.reveal'), icon: { key: 'open-in-app.reveal' },
                    disabled: !directory || manager === undefined, failureLabel: t('path.revealError'),
                    ...(directory && manager !== undefined ? { run: async () => {
                            const outcome = await controller.launch(manager, directory);
                            report(outcome.kind === 'failed' ? { kind: 'failed', failure: 'revealError' } : outcome);
                        } } : {}),
                }];
        },
    }), 'open-in-app: Session directory menu');
}
//# sourceMappingURL=session-reveal.js.map
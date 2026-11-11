import { NativeDirectoryFlow } from "./flow.js";
/** Required services (cordis fiber inject): the slot registry, workspace UI service, and access. */
export const inject = ['slots', 'uiWorkspace', 'remoteAccess'];
/**
 * Client plugin body: while the current Peer may open the Host's directory
 * chooser, register the renderless native flow into both directory-flow holes
 * through `slots.inject()` because the ui-workspace entries may activate later
 * or replace their declarations.
 * @param ctx - client root context.
 */
export function apply(ctx) {
    const desktop = globalThis.__CODESMITH_DIRECTORY_PICKER__;
    const pick = desktop === undefined ? () => ctx.uiWorkspace.pickDirectory() : () => desktop.pick();
    const injected = () => ({ pick });
    // Both declaration lifetimes must be live before the pair installs; the
    // generator makes the two registrations one transactional effect. The
    // outer/inner nesting order is arbitrary; neither hole has precedence.
    ctx.remoteAccess.whenAccess('directoryPicker/pick', () => ctx.slots.inject('conversation.hero.workspace.directoryFlow', () => ctx.slots.inject('sidebar.workspaces.directoryFlow', function* () {
        yield ctx.slots.register({
            name: 'conversation.hero.workspace.directoryFlow', inject: injected,
        }, NativeDirectoryFlow);
        yield ctx.slots.register({
            name: 'sidebar.workspaces.directoryFlow', inject: injected,
        }, NativeDirectoryFlow);
    })));
}
//# sourceMappingURL=index.js.map
import { createSnapshotStore, } from '@codesmith/client-store';
/**
 * Create the browser-wide trajectory duration preference source.
 * @returns a persisted source shared by every session view in one plugin lifecycle.
 */
export function createTrajectoryDurationStore() {
    return createSnapshotStore(false, {
        persist: { name: 'cds.trajectory.duration' },
    });
}
//# sourceMappingURL=duration-store.js.map
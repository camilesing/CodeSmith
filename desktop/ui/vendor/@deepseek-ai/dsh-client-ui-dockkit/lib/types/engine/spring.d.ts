/**
 * The spring a floating panel follows back inside the viewport after a release
 * past an edge. Pure and clock-free: the caller supplies each frame's elapsed
 * time, so trajectories are deterministic for a given sequence of frames.
 */
/** One axis of a spring: where it is and how fast it moves, in CSS pixels and pixels per second. */
export interface SpringAxis {
    readonly position: number;
    readonly velocity: number;
}
/** Fastest release velocity a return starts with, in pixels per second. */
export declare const SPRING_MAX_VELOCITY = 1500;
/** A return ends at its target after this long even if it is still moving. */
export declare const SPRING_DEADLINE_MS = 900;
/** A frame longer than this is clamped, so a slow frame cannot inject a jump. */
export declare const SPRING_MAX_FRAME_MS = 64;
/** A gap longer than this (sleep, a stalled tab) ends the return at its target instead of animating catch-up. */
export declare const SPRING_STALL_MS = 250;
/**
 * Advance one axis by an elapsed time, in equal substeps no longer than `SUBSTEP`
 * (semi-implicit Euler).
 * @param axis - the axis state.
 * @param target - the position the spring pulls toward.
 * @param dtSeconds - elapsed time, already clamped by the caller.
 * @returns the advanced state.
 */
export declare function stepSpring(axis: SpringAxis, target: number, dtSeconds: number): SpringAxis;
/**
 * The velocity an axis starts its return with. An axis already at its target
 * keeps still. Outward speed is capped at `omega0 * (reach - |offset|)`: the
 * spring carries a start offset `d` and speed `v` at most `d + 0.46 * v / omega0`
 * from its target, so the whole return stays within `reach`.
 * @param position - where the axis is drawn at release.
 * @param target - the position the spring pulls toward.
 * @param velocity - the drawn release velocity, in pixels per second.
 * @param reach - the farthest the return may be from the target.
 * @returns the starting velocity.
 */
export declare function launchVelocity(position: number, target: number, velocity: number, reach: number): number;
/**
 * Whether an axis has come to rest at its target.
 * @param axis - the axis state.
 * @param target - the spring's target.
 * @returns true within `REST_DISTANCE` of the target and slower than `REST_SPEED`.
 */
export declare function springSettled(axis: SpringAxis, target: number): boolean;
//# sourceMappingURL=spring.d.ts.map
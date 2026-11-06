/**
 * The Peer: one party admitted to this Host. Its creator fixes the principal
 * it speaks for and owns its lifetime; Gateway hands it to each Remote call as
 * `invocation.peer`.
 * @module @deepseek-ai/dsh-client-connection/src/peer
 */
import { randomUUID } from 'node:crypto';
import { createScope } from '@deepseek-ai/dsh-scope';
/**
 * One Peer's scope. The instance is its own scope key, so `scopeOf(peer.ctx)`
 * returns it and events dispatched with `scopeTarget(subject, peer)` reach
 * listeners registered through `peer.ctx` and nobody else.
 */
export class Peer {
    id = randomUUID();
    ctx;
    principal;
    interactions;
    signal;
    scope;
    /**
     * @param owner - Context the scope fiber hangs under; disposing it disposes the Peer.
     * @param options - the principal fixed for this Peer's lifetime.
     */
    constructor(owner, options) {
        this.principal = options.principal;
        this.interactions = options.interactions ?? 'answer';
        this.scope = createScope(owner, this);
        this.ctx = this.scope.ctx;
        const lifetime = new AbortController();
        this.signal = lifetime.signal;
        // Runs however the scope ends: dispose() or the owner Context unloading.
        this.ctx.effect(() => () => { lifetime.abort(new Error(`Peer ${this.id} was disposed`)); }, 'client-connection: Peer lifetime');
    }
    /** Tear down every connection-lifetime registration; racing calls share one completion. */
    dispose() {
        return this.scope.dispose();
    }
}
//# sourceMappingURL=peer.js.map
import { SecretText } from "./SecretText.js";
import { createSecretRule } from "./secret-rule.js";
import { en, ja, es, fr, de, ru, pt, NS, zh } from "./locales.js";
/** Required services: the copy, the node renderer registry, and the UserText rule registry. */
export const inject = ['locale', 'slots', 'uiConversation'];
/**
 * Register the dictionaries, the credential matcher, and its node renderer.
 * @param ctx - client root context.
 */
export function apply(ctx) {
    ctx.effect(() => ctx.locale.register(NS, { zh, en, ja, es, fr, de, ru, pt }), 'ui-secret-mask: dictionaries');
    ctx.uiConversation.userText.register(createSecretRule());
    ctx.slots.inject('conversation.user.text.node', () => ctx.slots.register({
        name: 'conversation.user.text.node', key: 'secret', priority: 100, locale: NS,
    }, SecretText));
}
//# sourceMappingURL=index.js.map
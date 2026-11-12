import { QuoteSelectionAction } from "./QuoteSelection.js";
import { QuotedUserText } from "./QuotedUserText.js";
import { quoteRule } from "./quote-rule.js";
import { en, ja, es, fr, de, ru, pt, NS, zh } from "./locales.js";
/** Browser plugin name. */
export const name = 'message-quotes';
/** Services needed to admit selections and register Quote presentation. */
export const inject = ['slots', 'locale', 'sessions', 'uiConversation'];
/**
 * Register Quote admission, its action surface, and literal UserText rendering.
 * @param ctx - Client root context owning every Quote contribution.
 */
export function apply(ctx) {
    ctx.effect(() => ctx.locale.register(NS, { zh, en, ja, es, fr, de, ru, pt }), 'message-quotes: dictionaries');
    ctx.effect(() => ctx.uiConversation.userText.register(quoteRule), 'message-quotes: UserText rule');
    ctx.effect(() => ctx.uiConversation.interactions.register({
        id: 'quote',
        onInteraction(request) {
            if (request.trigger !== 'selection' || request.readText() === null)
                return;
            const binding = ctx.sessions.binding(request.sessionId);
            if (binding === undefined)
                return;
            const session = binding.session.getSnapshot();
            if (session.removed || (session.subagent !== null
                && session.subagent.address.mode !== 'continuable'))
                return;
            request.open();
        },
    }), 'message-quotes: selection interaction');
    ctx.slots.inject('conversation.chat.text-interaction', () => ctx.slots.register({
        name: 'conversation.chat.text-interaction', key: 'quote', locale: NS,
    }, QuoteSelectionAction));
    ctx.slots.inject('conversation.user.text.node', () => ctx.slots.register({
        name: 'conversation.user.text.node', key: 'quote', priority: 100,
    }, QuotedUserText));
}
//# sourceMappingURL=index.js.map
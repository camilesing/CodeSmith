/**
 * CodeSmith brand plugin: a first-party row in the DSH client's plugin graph.
 *
 * The DSH hero renders its whale mark as the FALLBACK of the
 * `conversation.hero.brand.mark` factory slot — registering that slot (with a
 * null-rendering component) suppresses the fallback through the official
 * seam, so no vendored code is touched to remove the icon. Registration rides
 * `slots.inject`, which waits for the slot's declaration by ui-conversation
 * (the same pattern every DSH extension plugin uses). The headline text lives
 * in ui-conversation's locale dictionaries, which cannot be re-registered
 * (the locale service throws on a duplicate namespace+locale); that one
 * string is handled by the vendor patch pipeline instead.
 */

interface SlotsService {
  inject: (slot: string, register: () => unknown) => unknown
  register: (definition: { name: string; id: string }, component: (props: unknown) => null) => unknown
}

export const inject = ['slots']

export function apply(ctx: { slots: SlotsService }): void {
  ctx.slots.inject('conversation.hero.brand.mark', () => {
    ctx.slots.register(
      { name: 'conversation.hero.brand.mark', id: 'codesmith-brand' },
      () => null,
    )
  })
}

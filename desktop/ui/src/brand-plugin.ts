/**
 * CodeSmith brand plugin: a first-party row in the DSH client's plugin graph.
 *
 * The DSH whale marks (hero headline fish, sidebar header fish) render as
 * FALLBACKS of their factory slots — registering those slots (with a
 * null-rendering component) suppresses the fallbacks through the official
 * seam, so no vendored code is touched to remove the icons. Registration rides
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

/** The DSH whale marks suppressed through their factory-slot fallbacks. */
const BRAND_MARK_SLOTS = [
  'conversation.hero.brand.mark', // hero headline fish (EmptyHero)
  'sidebar.brand.mark', // sidebar header fish, collapsed rail + expanded row
] as const

export function apply(ctx: { slots: SlotsService }): void {
  for (const slot of BRAND_MARK_SLOTS) {
    ctx.slots.inject(slot, () => {
      ctx.slots.register({ name: slot, id: 'codesmith-brand' }, () => null)
    })
  }
}

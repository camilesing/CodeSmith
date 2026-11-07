/** Shared provider display order for the composer and command model pickers. */

/**
 * Put the account and official providers first, preserving every other relative order.
 * @param groups - Provider groups in catalog order.
 * @returns a sorted copy; model order within each group is unchanged.
 */
export function orderModelProviders<T extends { readonly id: string }>(groups: readonly T[]): T[] {
  return groups.toSorted((left, right) =>
    (left.id === 'codesmith-account' ? 0 : left.id === 'codesmith-official' ? 1 : 2)
      - (right.id === 'codesmith-account' ? 0 : right.id === 'codesmith-official' ? 1 : 2))
}

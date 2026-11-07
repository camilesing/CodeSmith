import { builtinCommandName } from '@codesmith/commands/builtin';
/**
 * Select the input spelling for a menu-picked command.
 * @param descriptor - effective Host descriptor.
 * @param t - command-namespace translator.
 * @returns localized spelling for a known definition, otherwise its registered name.
 */
export function claimToken(descriptor, t) {
    const name = builtinCommandName(descriptor);
    return name === undefined ? descriptor.name : t(`token.${name}`);
}
//# sourceMappingURL=resolution.js.map
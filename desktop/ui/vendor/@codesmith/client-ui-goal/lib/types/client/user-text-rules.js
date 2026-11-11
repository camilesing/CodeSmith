/** Return the nearest exact confirmed command, checking boundaries against the original string. */
export const goalCommandReferenceRule = {
    id: 'command',
    order: 20,
    match(text, from, context) {
        const slash = /(?<!\S)\//gu;
        slash.lastIndex = from;
        if (!slash.test(text))
            return undefined;
        const commands = context.read('commands');
        let best;
        for (const { start, end, name } of commands ?? []) {
            if (start < from || end > text.length || text.slice(start, end) !== `/${name}`
                || (start !== 0 && !/\s/u.test(text.slice(start - 1, start)))
                || (end !== text.length && !/\s/u.test(text.slice(end, end + 1))))
                continue;
            if (best === undefined || start < best.start || (start === best.start && end > best.end)) {
                best = { id: 'command', start, end, data: { name } };
            }
        }
        return best;
    },
};
//# sourceMappingURL=user-text-rules.js.map
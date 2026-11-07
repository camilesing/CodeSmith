/** Return one nearest confirmed slash token, retaining its original word boundaries. */
export const skillReferenceRule = {
    id: 'skill',
    order: 20,
    match(text, from, context) {
        const tokens = /(?<!\S)\/([\w-]+)(?!\S)/gu;
        tokens.lastIndex = from;
        let names;
        for (let token = tokens.exec(text); token !== null; token = tokens.exec(text)) {
            if (names === undefined) {
                names = context.read('skillNames');
                if (names === undefined || names.length === 0)
                    return undefined;
            }
            const name = token[1];
            if (names.includes(name))
                return {
                    id: 'skill', start: token.index, end: token.index + token[0].length, data: { name },
                };
        }
        return undefined;
    },
};
//# sourceMappingURL=user-text-rules.js.map
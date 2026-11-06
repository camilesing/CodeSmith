/** Bounded calendar recurrence shared by consumers of Cron and RRULE. */
import { fourDigitYearEpoch, isFourDigitYearInstant } from "./instant.js";
/** Internal validation failure, contained by the public parsers. */
export class RecurrenceInputError extends Error {
    reason;
    constructor(reason, message, options) {
        super(message, options);
        this.reason = reason;
    }
}
/**
 * Contain recurrence validation failures while preserving unexpected implementation errors.
 * @param parse - Synchronous rule constructor whose input failures use RecurrenceInputError.
 * @returns The parsed value or its validation refusal.
 */
export function parseRecurrence(parse) {
    try {
        return { kind: 'accepted', rule: parse() };
    }
    catch (error) {
        if (!(error instanceof RecurrenceInputError))
            throw error;
        return { kind: 'rejected', reason: error.reason, detail: error.message,
            ...(error.cause === undefined ? {} : { cause: error.cause }) };
    }
}
/**
 * Validate the explicit decision and its committed lower bound before calendar arithmetic.
 * @param scheduledAt - Committed canonical four-digit-year UTC occurrence with milliseconds.
 * @param acceptedAt - Explicit epoch milliseconds of the delivery decision.
 * @returns The committed epoch, or throws RangeError for an invalid decision.
 */
export function recurrenceDecision(scheduledAt, acceptedAt) {
    const savedTarget = fourDigitYearEpoch(scheduledAt);
    if (!isFourDigitYearInstant(acceptedAt) || savedTarget.kind === 'rejected' || acceptedAt < savedTarget.epoch) {
        throw new RangeError('Recurrence decision must be representable and at or after its committed target');
    }
    return savedTarget.epoch;
}
//# sourceMappingURL=recurrence.js.map
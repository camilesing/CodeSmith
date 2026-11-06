import { createSnapshotStore, } from '@deepseek-ai/dsh-client-store';
import { serializeDraftPrompt } from "./prompt.js";
import { SubmitMachine } from "./machine.js";
import { resolveDraftInput, snapshotDraft } from "../draft.js";
import { DraftEditorRuntime } from "./editor/runtime.js";
/** Guard tier from the machine phase. */
function guardOf(phase) {
    switch (phase) {
        case 'plain': return 'plain';
        case 'claimed': return 'claimed';
        default: return 'frozen'; // adjudicating / submitting / sending
    }
}
/** Whether an attempt owns the draft: the editor and attachments are read-only until it settles. */
function frozen(phase) {
    return phase === 'adjudicating' || phase === 'submitting' || phase === 'sending';
}
/** Whether two projections differ in content (selection and caret excluded). */
function projectionContentChanged(prev, next) {
    if (prev.clipboardText !== next.clipboardText || prev.detectText !== next.detectText)
        return true;
    if (prev.command !== next.command)
        return true;
    if (prev.quotes.length !== next.quotes.length || prev.quotes.some((quote, i) => {
        const old = next.quotes[i];
        return old?.text !== quote.text || old.offset !== quote.offset;
    }))
        return true;
    if (prev.occurrences.length !== next.occurrences.length)
        return true;
    return next.occurrences.some((occ, i) => {
        const old = prev.occurrences[i];
        return old === undefined || old.occurrenceId !== occ.occurrenceId || old.invalid !== occ.invalid;
    });
}
/** Message of a failure surfaced as a composer notice. */
function errorText(error) {
    return error instanceof Error ? error.message : String(error);
}
const EMPTY_QUEUE = [];
/** Unavailable catalogs contain no named text references. */
const EMPTY_LEXICON = new Map();
/**
 * The per-session input facade: scoped-event application verbs +
 * setDraft/submit + the published InputState store, over a shell-owned
 * Lexical editor.
 */
export class SessionInputShell {
    deps;
    /** Published editor projection + submit-plane state + queue overlay (the InputZone currency source). */
    state;
    /** Latest surfaced notice (null after clear); the bar renders errors as banners and information inline. */
    notices = createSnapshotStore(null);
    /** The shell-owned editor (text + chip truth); the composer binds its contenteditable to it. */
    get editor() {
        return this.draftEditor.editor;
    }
    /** The public provide-channel action face (one stable identity per session). */
    actions = {
        insertQuote: text => this.insertQuote(text),
        captureInsertion: () => ({ ...this.caretSpan(), draftRev: this.rev }),
        insertText: (text, span) => {
            if (frozen(this.snapshot.phase) || this.disposed)
                return false;
            if (span.draftRev !== this.rev)
                return false;
            return this.draftEditor.insertAsyncText(span, text);
        },
        setDraft: (text) => { this.setDraft(text); },
        persistDraft: () => { this.persistCurrentDraft(); },
        addAttachments: ids => this.addAttachments(ids),
        removeAttachment: (id) => { this.removeAttachment(id); },
        pruneAttachments: (ids) => { this.pruneAttachments(ids); },
        submit: () => { this.submit('queue'); },
    };
    core = new SubmitMachine();
    draftEditor;
    get projection() {
        return this.draftEditor.projection;
    }
    rev = 0;
    unregister;
    noticeSeq = 0;
    lastPublishedDraft;
    draftSnapshotCache;
    attachmentIds = [];
    disposed = false;
    /** Conversation store writer for the current semantic document. */
    persistDraft;
    /** A restored draft's command token waiting for the trigger pipeline, with the revision its restore produced. */
    pendingCommand;
    /** The in-flight claim resolution for a restored command chip. */
    commandRestore;
    /** The mounted composer's file-picker opener (scoped pick-files event target). */
    filePicker;
    unsubscribeInbox;
    constructor(deps) {
        this.deps = deps;
        this.draftEditor = new DraftEditorRuntime({
            onUpdate: () => { this.onEditorUpdate(); },
            quoteLabels: () => ({ remove: this.deps.quoteRemoveLabel?.() ?? '' }),
            openReference: (source, reference) => this.deps.inputTriggers?.()?.openReference(source, reference) ?? false,
            lexicon: () => this.lexicon.getSnapshot(),
            resolveLexicon: () => this.deps.inputTriggers?.()?.lexicon,
        });
        this.unregister = this.draftEditor.register();
        this.state = createSnapshotStore(this.compose());
        this.unsubscribeInbox = deps.inbox?.subscribe(() => { this.publish(); });
    }
    // ---- editor plumbing ----
    /** Re-project, sync the claim with the leading command chip, publish, and feed trigger tracking after every editor commit. */
    onEditorUpdate() {
        const prev = this.draftEditor.refreshProjection();
        // Selection-only commits advance neither the revision nor the published
        // state: menus still track the caret below, while draftRev moves only
        // with content so a snapshot-built span (apply.ts) stays CAS-valid across
        // caret motion and subscribers do not re-render per caret move.
        if (projectionContentChanged(prev, this.projection)) {
            this.rev += 1;
            this.dispatchRun(({ type: 'draft-changed', command: this.projection.command }));
        }
        const caret = this.projection.caret;
        if (caret !== null) {
            this.deps.inputTriggers?.()?.track(this.projection.detectText, caret, { tier: guardOf(this.core.state.phase) }, this.rev);
        }
    }
    // ---- SessionInput face ----
    /**
     * Replace the whole draft (persisted-draft seed and programmatic writes).
     * Placeholder-sanitized; newlines split paragraphs; the caret lands at the
     * end. Merged into history so a seed is not an undoable step of its own.
     * @param text - plain text or the complete semantic document.
     */
    setDraft(text) {
        const draft = resolveDraftInput(text);
        this.cancelCommandRestore();
        if (draft.references.length === 0 && (draft.quotes?.length ?? 0) === 0)
            this.draftEditor.setDraft(draft.text);
        else
            this.draftEditor.restoreDraft(draft.text, draft.references, draft.quotes, { remove: this.deps.quoteRemoveLabel?.() ?? '' });
        if (draft.command === undefined)
            return;
        this.pendingCommand = { token: draft.command.token, rev: this.rev };
        this.restoreCommand();
    }
    /** @param text - selected transcript text. @returns whether the editable draft accepted the quote. */
    insertQuote(text) {
        if (this.disposed || this.snapshot.phase !== 'plain' || this.deps.quoteInsertionAllowed?.() === false || text.trim() === '')
            return false;
        const inserted = this.draftEditor.insertQuote(text.replace(/\r\n?/gu, '\n'), { remove: this.deps.quoteRemoveLabel?.() ?? '' });
        if (inserted)
            this.focus();
        return inserted;
    }
    /**
     * Reconnect optional catalog notifications and resume a pending command
     * chip restore without waiting for another edit.
     */
    refreshLexiconSubscription() {
        if (this.disposed)
            return;
        this.draftEditor.refreshLexiconSubscription();
        this.restoreCommand();
    }
    /**
     * Rebuild the command chip a restored draft carried once the trigger
     * pipeline can resolve its claim. The rebuild applies only at the revision
     * the restore produced: an edit made while the claim resolves keeps the
     * literal token, which Enter still adjudicates as the command.
     */
    restoreCommand() {
        const pending = this.pendingCommand;
        const inputTriggers = this.deps.inputTriggers?.();
        if (pending === undefined || inputTriggers === undefined || this.disposed)
            return;
        this.pendingCommand = undefined;
        const restore = new AbortController();
        this.commandRestore = restore;
        const spelling = pending.token.trimEnd();
        inputTriggers.resolveClaim(spelling, restore.signal).then((claim) => {
            if (restore.signal.aborted)
                return;
            this.commandRestore = undefined;
            if (claim !== undefined)
                this.applyClaim(claim, { start: 0, end: spelling.length, draftRev: pending.rev }, true);
        }, (_error) => {
            // A catalog warmup failure or an abort leaves the literal token, which Enter still adjudicates.
            if (this.commandRestore === restore)
                this.commandRestore = undefined;
        });
    }
    /** Drop a pending or in-flight command chip restore. */
    cancelCommandRestore() {
        this.pendingCommand = undefined;
        this.commandRestore?.abort();
        this.commandRestore = undefined;
    }
    /** Current semantic document, stable until its content revision changes. */
    get draftSnapshot() {
        if (this.draftSnapshotCache?.revision === this.rev)
            return this.draftSnapshotCache.value;
        const value = snapshotDraft(this.projection.clipboardText, this.projection.occurrences, this.projection.command?.token, this.projection.quotes);
        this.draftSnapshotCache = { revision: this.rev, value };
        return value;
    }
    /** Persist the latest document without changing the editor or binding a writer. */
    persistCurrentDraft() {
        this.persistDraft?.(this.draftSnapshot);
    }
    /**
     * Apply one new-task request to this already-initialized input model.
     * @param options - content to adopt and the handling of an existing draft.
     * @returns applied when the requested text is adopted, even if unchanged;
     * preserved when the current draft is kept; blocked after disposal or during a pending submission.
     */
    requestDraftInitialization(options) {
        if (this.draftInitializationBlocked())
            return 'blocked';
        const whenDraftExists = options.whenDraftExists ?? (options.clearPreviousDraft === true ? 'replace' : 'keep');
        if (whenDraftExists === 'replace') {
            this.setDraft(options.prompt ?? '');
            return 'applied';
        }
        if (options.prompt === undefined)
            return 'preserved';
        if (this.snapshot.draft === '' && this.attachmentIds.length === 0) {
            this.setDraft(options.prompt);
            return 'applied';
        }
        if (whenDraftExists === 'keep')
            return 'preserved';
        this.draftEditor.appendDraft(options.prompt);
        return 'applied';
    }
    draftInitializationBlocked() {
        return this.disposed || frozen(this.core.state.phase);
    }
    /** Append ordered attachment ids unless an admission transaction is locked. */
    addAttachments(ids) {
        if (frozen(this.snapshot.phase))
            return false;
        if (ids.length === 0)
            return true;
        this.attachmentIds = [...this.attachmentIds, ...ids];
        this.publish();
        return true;
    }
    /**
     * Add validated file references and attachment ids while admission is editable.
     * @param references - reference chips in source order.
     * @param ids - newly allocated attachment ids.
     * @returns false when admission is locked or the editor refuses the insertion.
     */
    addFiles(references, ids) {
        if (frozen(this.snapshot.phase))
            return false;
        if (!this.draftEditor.insertFileReferences(references))
            return false;
        this.attachmentIds = [...this.attachmentIds, ...ids];
        this.publish();
        return true;
    }
    /**
     * Remove one attachment id from this draft. Busy admission phases refuse, like
     * {@link addAttachments}: a removal landing while a send serializes would
     * otherwise vanish from the rail yet still ride the in-flight send.
     */
    removeAttachment(id) {
        if (frozen(this.snapshot.phase))
            return false;
        const next = this.attachmentIds.filter(candidate => candidate !== id);
        if (next.length === this.attachmentIds.length)
            return false;
        this.attachmentIds = next;
        this.publish();
        return true;
    }
    /**
     * Keep only ids that still resolve in the browser attachment registry.
     * @param available - live registry ids.
     */
    pruneAttachments(available) {
        const keep = new Set(available);
        const next = this.attachmentIds.filter(id => keep.has(id));
        if (next.length === this.attachmentIds.length)
            return;
        this.attachmentIds = next;
        this.publish();
    }
    /**
     * Insert pasted plain text over the current editor selection
     * (placeholder-sanitized). The paste event's own default is suppressed by
     * the caller; PASTE_TAG makes the paste its own history boundary, so one
     * undo never removes both the paste and typing inside the merge window.
     * @param text - pasted plain text.
     */
    paste(text) {
        this.draftEditor.paste(text);
    }
    /**
     * Enter adjudication + submit transaction + default sink with the draft, its
     * reference chips, and its attachments as the user sent them. Effects fan
     * out from the machine; this method only feeds the event. Lock entry
     * (adjudicating/submitting/sending) force-closes the transient layers: the
     * popup dismisses and the menu tracks frozen.
     */
    submit(mode = 'queue', source) {
        if (this.disposed)
            return;
        const timestamp = Date.now();
        let state;
        if (this.snapshot.phase === 'plain' && (this.snapshot.draft.trim() !== '' || this.attachmentIds.length > 0)) {
            try {
                state = this.deps.submissionState?.();
            }
            catch (_error) { /* Optional Session observations cannot interrupt submission. */ }
        }
        const submission = Object.freeze({
            timestamp, mode, ...source === undefined ? {} : { source }, ...state === undefined ? {} : { state },
        });
        // Claimed pre-gate: a claim that does not declare attachment acceptance never
        // submits while attachments are present — one notice, everything retained.
        // Enter-time adjudication applies the same policy for unclaimed lines
        // inside the command source itself.
        const before = this.snapshot;
        if (before.phase === 'claimed' && this.attachmentIds.length > 0 && before.claim?.attachments !== true) {
            this.notify('error', this.deps.commandAttachments.unsupportedNotice(before.claim?.token ?? before.draft));
            return;
        }
        this.dispatchRun({
            type: 'enter',
            mode,
            draft: this.projection.clipboardText,
            occurrences: [...this.projection.occurrences],
            quotes: [...this.projection.quotes],
            attachmentIds: [...this.attachmentIds],
            submission,
        });
        if (frozen(this.snapshot.phase)) {
            this.deps.popup?.()?.dismiss();
            this.deps.inputTriggers?.()?.track(this.projection.detectText, 0, { tier: 'frozen' }, this.rev);
        }
    }
    /**
     * Keyboard arbitration while the menu is open.
     * @param key - the intercepted key.
     * @param composing - IME composition guard state.
     * @returns the menu's verdict; 'pass' when no pipeline is mounted.
     */
    arbitrate(key, composing) {
        return this.deps.inputTriggers?.()?.arbitrate(key, composing) ?? 'pass';
    }
    /**
     * Steer every still-pending queued message into the running turn (the
     * empty-draft accelerated-Enter gesture). Execution belongs to the hub's
     * queue choreography; absent dep = the gesture falls back to the machine's
     * empty-draft no-op.
     */
    steerQueue() {
        this.deps.steerQueue?.();
    }
    /**
     * Space adjudication over the controller's hot state.
     * @returns true = a claim/insert was applied — the caller preventDefaults.
     */
    space() {
        const inputTriggers = this.deps.inputTriggers?.();
        if (inputTriggers === undefined)
            return false;
        return inputTriggers.onSpace();
        // No re-track here: applying the claim/insert mutates the editor, and the
        // update listener re-tracks at the settled caret on its own.
    }
    /** Dismiss the popupSelect shell (any interaction outside the box). */
    dismissPopup() {
        this.deps.popup?.()?.dismiss();
    }
    /**
     * The live selection as a detect-coordinate span (menu-launcher synthetic
     * hits replace it on pick); an absent selection answers a collapsed span at
     * the document end.
     * @returns the ordered [start, end) span in detect coordinates.
     */
    caretSpan() {
        return this.draftEditor.caretSpan();
    }
    /**
     * Hot plain-text reference lexicon source for the decoration scan:
     * delegates to the controller's aggregated store. Stable
     * identity per shell; without a pipeline the snapshot is the empty Map and
     * subscribers never fire.
     */
    lexicon = {
        getSnapshot: () => this.deps.inputTriggers?.()?.lexicon.getSnapshot() ?? EMPTY_LEXICON,
        subscribe: fn => this.deps.inputTriggers?.()?.lexicon.subscribe(fn) ?? (() => { }),
    };
    // ---- scoped-event application verbs ----
    /**
     * Apply one command claim (scoped begin-command event listener body): the
     * editor replaces [0, span.end) with the command chip and its separator,
     * then the machine enters claimed.
     * @param claim - the command claim from the pick path.
     * @param span - pick-time span snapshot (detect coordinates).
     * @returns whether the edit applied (phase, span CAS, and leading guard passed).
     */
    beginCommand(claim, span) {
        return this.applyClaim(claim, span, false);
    }
    /**
     * Replace [0, span.end) with the command chip and enter claimed.
     * @param mergeHistory - merge the edit into the previous history entry.
     * @returns whether the edit applied.
     */
    applyClaim(claim, span, mergeHistory) {
        const phase = this.core.state.phase;
        if (phase !== 'plain' && phase !== 'claimed')
            return false;
        if (span.draftRev !== this.rev)
            return false;
        // Leading-trigger contract: only whitespace may precede the span; the
        // whitespace prefix is dropped so the chip takes the leading seat.
        if (this.projection.detectText.slice(0, span.start).trim() !== '')
            return false;
        const tail = this.projection.detectText.slice(span.end, span.end + 1);
        const applied = this.draftEditor.insertCommand({ start: 0, end: span.end }, claim, tail, mergeHistory);
        if (!applied)
            return false;
        this.dispatchRun(({ type: 'claim', claim }));
        return true;
    }
    /**
     * Apply one reference insertion (scoped insert-reference event listener
     * body): the editor replaces the span with one chip node, followed by a
     * separating space unless one is already next.
     * @param ref - the reference insertion from the pick path.
     * @param span - pick-time span snapshot (detect coordinates).
     * @returns whether the edit applied.
     */
    insertReference(ref, span) {
        const phase = this.core.state.phase;
        if (phase !== 'plain' && phase !== 'claimed')
            return false;
        if (span.draftRev !== this.rev)
            return false;
        const tail = this.projection.detectText.slice(span.end, span.end + 1);
        return this.draftEditor.insertReference(span, ref, tail);
    }
    /**
     * Consume one command token after business success (scoped consume-token
     * event listener body). Span guard: revision CAS then splice; bare-token
     * guard: trimmed-draft equality then clear.
     * @param guard - exact span or bare-token guard.
     * @returns whether the token was consumed.
     */
    consumeToken(guard) {
        if (guard.kind === 'span') {
            if (guard.span.draftRev !== this.rev || guard.span.start === guard.span.end)
                return false;
            return this.draftEditor.replaceText(guard.span, '');
        }
        if (guard.token === '' || this.projection.clipboardText.trim() !== guard.token)
            return false;
        this.setDraft('');
        return true;
    }
    /**
     * Insert plain reference text over the pick-time span (scoped insert-text
     * event listener body; the plain-text reference path). The editor gains
     * ordinary characters — no chip node; the chip look is a scan-derived
     * decoration, never state.
     * @param text - the plain reference text to splice in (e.g. `/name `).
     * @param span - pick-time span snapshot (detect coordinates).
     * @param keepCompleting - contract passenger; completion re-opening is
     * automatic here (the update listener re-tracks at the settled caret, so an
     * open token — a directory pick's trailing slash — reopens the menu without
     * an explicit re-track).
     * @returns whether the text was applied.
     */
    insertText(text, span, keepCompleting = false) {
        void keepCompleting;
        if (span.draftRev !== this.rev)
            return false;
        return this.draftEditor.replaceText(span, text);
    }
    /**
     * Surface a notice from outside the machine (detached command results).
     * @param level - severity tier.
     * @param text - notice body.
     */
    notify(level, text) {
        this.noticeSeq += 1;
        this.notices.set({ level, text, seq: this.noticeSeq });
    }
    /**
     * Return the keyboard to the composer with the caret it last held. Lexical's
     * own focus restores its stored selection; a bare DOM focus on the
     * contenteditable would land the caret at the start instead.
     */
    focus() {
        this.editor.getRootElement()?.focus({ preventScroll: true });
        this.editor.focus();
    }
    // ---- wiring-layer extras (not on the frozen SessionInput face) ----
    /**
     * Teardown the shell, abort the in-flight attempt, and return every
     * browser-owned attachment the draft still holds, including those of a
     * message the Host has not accepted yet.
     * @returns attachment ids the scope disposer must release.
     */
    dispose() {
        if (this.disposed)
            return [];
        const retained = [...this.attachmentIds];
        this.disposed = true;
        this.cancelCommandRestore();
        this.dispatchRun(({ type: 'release' }));
        this.unsubscribeInbox?.();
        this.unregister();
        return retained;
    }
    /** Read the live input state (guard derivation reads here). */
    get snapshot() {
        return this.state.getSnapshot();
    }
    /**
     * Bind the Conversation store writer without importing or saving content.
     * @param write - semantic draft writer; persistCurrentDraft flushes the initial value.
     * @returns the unbind disposer.
     */
    bindDraftPersistence(write) {
        this.persistDraft = write;
        return () => {
            if (this.persistDraft === write)
                this.persistDraft = undefined;
        };
    }
    /**
     * Bind the mounted composer's file action and live intake availability.
     * @param picker - availability query and native file-dialog opener.
     * @returns the unbind disposer.
     */
    bindFilePicker(picker) {
        this.filePicker = picker;
        return () => {
            if (this.filePicker === picker)
                this.filePicker = undefined;
        };
    }
    /**
     * Read the mounted composer's live file-intake availability.
     * @returns false when no accepting composer is mounted.
     */
    canPickFiles() {
        return this.filePicker?.available() === true;
    }
    /**
     * Open the native file dialog when the mounted composer accepts files.
     * @returns whether the opener was called.
     */
    pickFiles() {
        if (this.filePicker === undefined || !this.filePicker.available())
            return false;
        this.filePicker.open();
        return true;
    }
    // ---- effect executor ----
    /** Dispatch one machine event and execute its effects. */
    dispatchRun(ev) {
        this.run(this.core.dispatch(ev));
    }
    run(effects) {
        for (const fx of effects)
            this.execute(fx);
        this.publish();
    }
    execute(fx) {
        switch (fx.type) {
            case 'notice': {
                this.noticeSeq += 1;
                this.notices.set({ level: fx.level, text: fx.text, seq: this.noticeSeq });
                return;
            }
            case 'adjudicate': {
                this.adjudicate(fx.attempt, fx.draft);
                return;
            }
            case 'begin-submit': {
                this.beginSubmit(fx.attempt, fx.claim, fx.args);
                return;
            }
            case 'default-sink': {
                this.sinkSerialized(fx.attempt, fx.draft, fx.mode);
                return;
            }
            case 'commit-draft': {
                this.commitDraft(fx.retainSuffixOf);
                return;
            }
        }
    }
    /**
     * Execute the commit-draft effect: clear the committed content (retaining
     * a pure typed-during-flight suffix when the snapshot allows) and cut the
     * undo history so sent content cannot resurrect.
     */
    commitDraft(retainSuffixOf) {
        this.draftEditor.clearCommittedDraft((clip) => {
            if (retainSuffixOf !== null && clip !== retainSuffixOf && clip.startsWith(retainSuffixOf)) {
                return retainSuffixOf.length;
            }
            return null;
        });
        this.draftEditor.clearHistory();
    }
    /**
     * Prompt serialization before the sink: expand each chip occurrence of the
     * sent draft to its owner's model form via the session controller's codec
     * routing and format each quote as Markdown, then send it with the
     * attachments captured at Enter. Owner missing or serialization failure
     * settles the send as failed, which keeps the draft. Chip-free drafts skip
     * the async detour.
     */
    sinkSerialized(attempt, draft, mode) {
        this.notifySubmission(attempt.submission);
        const { occurrences, quotes, attachmentIds } = attempt;
        const inputTriggers = this.deps.inputTriggers?.();
        const prompt = serializeDraftPrompt(snapshotDraft(draft, occurrences, undefined, quotes), async (source, ref) => {
            if (inputTriggers === undefined)
                throw new Error(`no serializer for reference source "${source}"`);
            return inputTriggers.serializeReference(source, ref, attempt.signal);
        });
        const send = (text) => {
            if (this.dead(attempt))
                return;
            this.settleSink(attempt, this.deps.defaultSink(text, attachmentIds, mode, attempt.signal, () => { this.acceptSink(attempt); }));
        };
        if (!(prompt instanceof Promise)) {
            send(prompt);
            return;
        }
        void prompt.then(send, (error) => { this.failSink(attempt, error); });
    }
    notifySubmission(submission) {
        if (submission === undefined)
            return;
        try {
            this.deps.messageSubmitted?.(submission);
        }
        catch (_error) { /* Notification consumers cannot interrupt submission. */ }
    }
    /**
     * Settle the message being sent. Once the Host accepts it, its attachments
     * leave the draft before the editor clears; ConversationController releases
     * them when the message's echo retires. Any other outcome keeps the draft
     * and its attachments for correction.
     */
    settleSink(attempt, pending) {
        pending.then((outcome) => { if (!this.dead(attempt))
            this.applySinkOutcome(attempt, outcome); }, (error) => { this.failSink(attempt, error); });
    }
    /** Settle the message being sent as accepted when the sink learns the Host's acceptance before its outcome. */
    acceptSink(attempt) {
        if (this.dead(attempt) || this.core.sendingAttempt !== attempt)
            return;
        this.applySinkOutcome(attempt, { kind: 'success' });
    }
    applySinkOutcome(attempt, outcome) {
        const accepted = outcome.kind === 'success';
        if (accepted && attempt.attachmentIds.length > 0) {
            const sent = new Set(attempt.attachmentIds);
            this.attachmentIds = this.attachmentIds.filter(id => !sent.has(id));
        }
        this.dispatchRun(({ type: 'sink-settled', attempt, ok: accepted, outcome }));
    }
    /** Settle a send that ended without a Host outcome; the draft stays for correction. */
    failSink(attempt, error) {
        if (this.dead(attempt))
            return;
        this.dispatchRun(({ type: 'sink-settled', attempt, ok: false, message: errorText(error) }));
    }
    /** Enter adjudication: poll the session controller; failure = notice + draft retained (never a silent downgrade). */
    adjudicate(attempt, draft) {
        const inputTriggers = this.deps.inputTriggers?.();
        if (inputTriggers === undefined) {
            // No pipeline mounted: the '/' line is an ordinary message.
            this.dispatchRun(({ type: 'adjudicated', attempt, outcome: undefined }));
            return;
        }
        inputTriggers.adjudicate(draft.trim(), attempt.signal, { attachments: this.attachmentIds.length }).then((outcome) => {
            if (this.dead(attempt))
                return;
            this.dispatchRun(({ type: 'adjudicated', attempt, outcome }));
        }, (error) => {
            if (this.dead(attempt))
                return;
            const message = error instanceof Error ? error.message : String(error);
            this.dispatchRun(({ type: 'adjudication-failed', attempt, message }));
        });
    }
    /**
     * The submit transaction: claim.submit against the session scope; ok maps
     * from the outcome kind. An accepting claim receives the serialized draft
     * attachments, which are cleared and released only on a success outcome; a
     * failure (serialize, transport, or handler error) keeps draft and attachments
     * for correction.
     */
    beginSubmit(attempt, claim, args) {
        const attachmentIds = claim.attachments === true ? attempt.attachmentIds : [];
        Promise.resolve()
            .then(async () => {
            const attachments = attachmentIds.length > 0 ? await this.deps.commandAttachments.serialize(attachmentIds) : [];
            // Serialization may outlive the attempt (large files, session
            // teardown); a dead attempt must not reach the Host executor.
            if (this.dead(attempt))
                return undefined;
            return claim.submit(args, this.deps.actx, attachments);
        })
            .then((outcome) => {
            if (outcome === undefined || this.dead(attempt))
                return;
            if (outcome.kind === 'success' && attachmentIds.length > 0) {
                const submitted = new Set(attachmentIds);
                this.attachmentIds = this.attachmentIds.filter(id => !submitted.has(id));
                this.deps.commandAttachments.release(attachmentIds);
            }
            this.dispatchRun(({
                type: 'submit-settled', attempt, ok: outcome.kind === 'success',
                command: this.projection.command, outcome,
                ...(outcome.kind === 'error' && outcome.text === undefined ? { message: 'command failed' } : {}),
            }));
        }, (error) => {
            if (this.dead(attempt))
                return;
            const message = error instanceof Error ? error.message : String(error);
            this.dispatchRun(({
                type: 'submit-settled', attempt, ok: false,
                command: this.projection.command, message,
            }));
        });
    }
    /** Late-settlement guard: superseded attempts and disposed facades drop silently. */
    dead(attempt) {
        return this.disposed || attempt.signal.aborted;
    }
    compose() {
        const core = this.core.state;
        return {
            draft: this.projection.clipboardText,
            attachmentIds: this.attachmentIds,
            draftRev: this.rev,
            phase: core.phase,
            ...(core.claim !== undefined ? { claim: core.claim } : {}),
            occurrences: this.projection.occurrences,
            queue: this.deps.inbox?.getSnapshot()?.['next-turn'] ?? EMPTY_QUEUE,
        };
    }
    publish() {
        const next = this.compose();
        this.state.set(next);
        const draft = this.draftSnapshot;
        if (draft !== this.lastPublishedDraft) {
            this.lastPublishedDraft = draft;
            this.persistDraft?.(draft);
        }
    }
}
//# sourceMappingURL=facade.js.map
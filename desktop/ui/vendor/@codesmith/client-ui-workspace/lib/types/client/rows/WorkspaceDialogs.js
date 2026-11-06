import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
/** Workspace rename and removal dialogs, keyed by their captured request. */
import { useEffect, useRef, useState } from 'react';
import { Button, Modal } from '@codesmith/client-ui-primitives';
import css from './WorkspaceBrowser.module.css';
/** @param props - captured request, current Workspace list, and existing mutations. @returns the active dialog. */
export function WorkspaceDialogs({ request, workspaces, renameWorkspace, deleteWorkspace, settleWorkspaceAction, t }) {
    const composingRef = useRef(false);
    const renameTarget = request.action === 'rename' ? request : null;
    const [renameDraft, setRenameDraft] = useState(request.displayTitle);
    const [renaming, setRenaming] = useState(false);
    const [renameError, setRenameError] = useState(null);
    const renameTrimmed = renameDraft.trim();
    // Self is excluded by identity, not by title: the draft is seeded with the
    // localized label, which for an automatically titled Workspace equals its
    // own displayed title without being a conflict with itself.
    const renameDuplicate = renameTarget !== null && renameTrimmed !== ''
        && workspaces.some(w => w.workspaceId !== renameTarget.workspaceId && w.title === renameTrimmed);
    const renameBlocked = renaming || renameTrimmed === ''
        || renameTarget === null || renameTrimmed === renameTarget.storedTitle || renameDuplicate;
    const closeRename = () => {
        if (renaming)
            return;
        settleWorkspaceAction();
        setRenameError(null);
    };
    const confirmRename = () => {
        if (renameBlocked)
            return;
        setRenaming(true);
        setRenameError(null);
        renameWorkspace(renameTarget.workspaceId, renameTrimmed).then(() => {
            setRenaming(false);
            settleWorkspaceAction();
        }).catch((reason) => {
            setRenaming(false);
            setRenameError(reason instanceof Error ? reason.message : String(reason));
        });
    };
    const deleteTarget = request.action === 'delete' ? request : null;
    const [deleting, setDeleting] = useState(false);
    const [deleteCommittedId, setDeleteCommittedId] = useState(null);
    const [deleteError, setDeleteError] = useState(null);
    useEffect(() => {
        if (deleteCommittedId === null
            || workspaces.some(workspace => workspace.workspaceId === deleteCommittedId))
            return;
        setDeleting(false);
        setDeleteCommittedId(null);
        settleWorkspaceAction();
    }, [deleteCommittedId, workspaces, settleWorkspaceAction]);
    const closeDelete = () => {
        if (deleting)
            return;
        settleWorkspaceAction();
        setDeleteError(null);
    };
    const confirmDelete = () => {
        /* v8 ignore next -- the Modal is absent without a target and its button is disabled while deleting. */
        if (deleting || deleteTarget === null)
            return;
        setDeleting(true);
        setDeleteCommittedId(null);
        setDeleteError(null);
        deleteWorkspace(deleteTarget.workspaceId).then(() => {
            // Keep the confirmation pending until this component has rendered the
            // committed list projection without the deleted id. Closing earlier
            // exposes one stale React frame to the next Create Workspace gesture.
            setDeleteCommittedId(deleteTarget.workspaceId);
        }).catch((reason) => {
            setDeleting(false);
            setDeleteError(reason instanceof Error ? reason.message : String(reason));
        });
    };
    return _jsxs(_Fragment, { children: [_jsxs(Modal, { open: renameTarget !== null, onClose: closeRename, closeLabel: t('close'), title: t('rename.workspace.title'), footer: (_jsxs(_Fragment, { children: [_jsx(Button, { variant: "outline", disabled: renaming, onClick: closeRename, children: t('cancel') }), _jsx(Button, { variant: "primary", disabled: renameBlocked, onClick: confirmRename, children: t('rename') })] })), children: [_jsx("input", { className: css.renameInput, value: renameDraft, "aria-label": t('field.workspaceName'), "data-modal-autofocus": true, disabled: renaming, onFocus: (e) => { e.target.select(); }, onChange: (e) => { setRenameDraft(e.target.value); setRenameError(null); }, onCompositionStart: () => { composingRef.current = true; }, onCompositionEnd: () => { composingRef.current = false; }, onKeyDown: (e) => {
                            if (e.key === 'Enter' && !composingRef.current) {
                                e.preventDefault();
                                confirmRename();
                            }
                        } }), renameDuplicate && (_jsx("div", { className: css.renameError, role: "alert", children: t('conflict.named', { name: renameTrimmed }) })), renameError !== null && _jsx("div", { className: css.renameError, role: "alert", children: renameError })] }), _jsxs(Modal, { open: deleteTarget !== null, onClose: closeDelete, closeLabel: t('close'), title: t('delete.workspace'), ...deleteTarget === null
                    ? {}
                    : { description: t('delete.desc', { name: deleteTarget.displayTitle }) }, footer: (_jsxs(_Fragment, { children: [_jsx(Button, { variant: "outline", disabled: deleting, onClick: closeDelete, children: t('cancel') }), _jsx(Button, { variant: "outline", className: css.deleteAction, disabled: deleting, onClick: confirmDelete, children: t('delete.workspace') })] })), children: [deleting && _jsx("div", { className: css.deleteStatus, role: "status", children: t('delete.pending') }), deleteError !== null && _jsx("div", { className: css.renameError, role: "alert", children: deleteError })] })] });
}
//# sourceMappingURL=WorkspaceDialogs.js.map
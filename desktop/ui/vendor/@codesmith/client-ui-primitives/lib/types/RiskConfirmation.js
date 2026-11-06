import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
/**
 * Controlled risk acknowledgement dialog shared by product surfaces that
 * must gate a sensitive action behind an explicit checkbox.
 */
import { Button } from "./Button.js";
import { Checkbox } from "./Checkbox.js";
import { Modal } from "./Modal.js";
import css from './RiskConfirmation.module.css';
/**
 * Render one in-page confirmation whose primary action is unavailable until
 * the caller-controlled acknowledgement is checked.
 */
export function RiskConfirmation({ open, title, description, acknowledgeLabel, cancelLabel, closeLabel, confirmLabel, acknowledged, disabled = false, onAcknowledgedChange, onCancel, onConfirm, }) {
    return (_jsxs(Modal, { open: open, onClose: onCancel, title: title, tone: "warning", closeLabel: closeLabel, className: css.confirmation ?? '', contentClassName: css.confirmationContent ?? '', footer: (_jsxs(_Fragment, { children: [_jsx(Button, { variant: "outline", className: css.modalAction, onClick: onCancel, children: cancelLabel }), _jsx(Button, { variant: "primary", className: css.confirmAction, disabled: disabled || !acknowledged, onClick: onConfirm, children: confirmLabel })] })), children: [_jsx("p", { className: css.description, children: description }), _jsx(Checkbox, { className: css.acknowledgement, checked: acknowledged, disabled: disabled, modalAutofocus: true, label: acknowledgeLabel, onChange: onAcknowledgedChange })] }));
}
//# sourceMappingURL=RiskConfirmation.js.map
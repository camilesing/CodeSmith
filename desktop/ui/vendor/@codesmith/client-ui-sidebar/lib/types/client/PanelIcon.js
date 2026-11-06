/**
 * Render the icon registered for one panel at the caller's size and selection state.
 * @param props - Panel identity, icon presentation, and the authorized child renderer.
 * @returns The matching panel icon.
 */
export function PanelIcon({ id, size, active, renderSlot }) {
    return renderSlot('sidebar.panellist', { size, active }, { only: id });
}
//# sourceMappingURL=PanelIcon.js.map
/**
 * Mark the pane a flash request names, docked or floating, until its animation ends.
 * @param root - the Session's panel, which contains its docked and floating panes.
 * @param flash - the latest request; alternating serials select alternating keyframes, so a repeat restarts.
 * @returns the cleanup that removes the mark, or undefined when no pane was marked.
 */
export function flashPane(root, flash) {
    if (root === null || flash === undefined)
        return undefined;
    const pane = root.querySelector(`[data-dockkit-pane="${flash.paneId}"], [data-dockkit-float="${flash.paneId}"]`);
    if (pane === null)
        return undefined;
    // A repeat on the same pane cancels the previous keyframes after this mark is set, so only this glow's own animation ends it.
    let started;
    const start = (event) => { if (event.target === pane)
        started = event.animationName; };
    const end = (event) => { if (event.target === pane && event.animationName === started)
        clear(); };
    const clear = () => {
        pane.removeEventListener('animationstart', start);
        pane.removeEventListener('animationend', end);
        pane.removeEventListener('animationcancel', end);
        delete pane.dataset.sidebarRightFlash;
    };
    pane.dataset.sidebarRightFlash = flash.serial % 2 === 0 ? 'even' : 'odd';
    pane.addEventListener('animationstart', start);
    pane.addEventListener('animationend', end);
    pane.addEventListener('animationcancel', end);
    return clear;
}
//# sourceMappingURL=flash.js.map
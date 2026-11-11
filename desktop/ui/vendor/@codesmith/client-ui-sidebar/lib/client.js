window.__ModuleLoader__.load({
	id: "@codesmith/client-ui-sidebar",
	factory: (require) => {
		var module = { exports: {} };
		var exports = module.exports;
		Object.defineProperty(exports, Symbol.toStringTag, { value: "Module" });
		let _codesmith_client_store = require("@codesmith/client-store");
		let _codesmith_client_ui_slots = require("@codesmith/client-ui-slots");
		let react_jsx_runtime = require("react/jsx-runtime");
		let _codesmith_client_ui_primitives = require("@codesmith/client-ui-primitives");
		let react = require("react");
		const NO_PANELS = [];
		/**
		* Create the in-memory panel pin list; the snapshot keeps its reference until the list changes.
		* @returns Unpinned panel ids and the pin mutation.
		*/
		function createPanelPins() {
			const store = (0, _codesmith_client_store.createSnapshotStore)({ unpinned: NO_PANELS });
			return {
				unpinned: {
					getSnapshot: () => store.getSnapshot().unpinned,
					subscribe: (listener) => store.subscribe(listener)
				},
				setPinned: (id, pinned) => {
					const current = store.getSnapshot().unpinned;
					if (id === "plugins" || pinned !== current.includes(id)) return;
					store.set({ unpinned: pinned ? current.filter((entry) => entry !== id) : [...current, id] });
				}
			};
		}
		//#endregion
		//#region \0cds-css:/upstream/packages/client/ui-sidebar/src/client/HeaderLeadingControls.module.css.mjs
		const css$1 = ".ZceueG_controls{align-items:center;gap:8px;display:flex}.ZceueG_iconButton{border-radius:var(--dsw-radius-sm);width:28px;height:28px;color:var(--dsw-alias-label-secondary);cursor:pointer;background:0 0;border:none;flex:none;justify-content:center;align-items:center;padding:0;display:inline-flex}@media (hover:hover){.ZceueG_iconButton:hover{background:var(--dsw-alias-interactive-bg-hover)}}";
		const tagId$1 = "@codesmith/client-ui-sidebar/HeaderLeadingControls.module.css";
		if (typeof document !== "undefined" && document.querySelector("style[data-plugin-css=" + JSON.stringify(tagId$1) + "]") === null) {
			const tag = document.createElement("style");
			tag.dataset.plugin = "@codesmith/client-ui-sidebar";
			tag.dataset.pluginCss = tagId$1;
			tag.textContent = css$1;
			document.head.appendChild(tag);
		}
		var HeaderLeadingControls_module_css_default = {
			"controls": "ZceueG_controls",
			"iconButton": "ZceueG_iconButton"
		};
		//#endregion
		//#region lib/types/client/HeaderLeadingControls.js
		/** Window-chrome controls for the fully hidden sidebar (frame shell.leading seat). */
		/**
		* Sidebar-open and New Session controls in the frame's window-chrome seat.
		* The frame mounts this seat for hidden sidebars and owns its placement.
		* Web shows only the sidebar opener; Desktop also retains New Session.
		* @param props - Injected sidebar actions, the global access Hook, and the sidebar locale seat.
		* @returns the sidebar toggle, plus New Session on Desktop while access allows `session/create`.
		*/
		function HeaderLeadingControls({ toggleSidebar, startSession, trackEntryClick, useRemoteAccess, useShortcuts, t }) {
			const canStartSession = useRemoteAccess("session/create");
			const desktop = document.documentElement.hasAttribute("data-platform");
			const shortcut = useShortcuts((rows) => rows.find((row) => row.id === "sidebar.left.toggle"));
			const newShortcut = useShortcuts((rows) => rows.find((row) => row.id === "session.new"));
			return (0, react_jsx_runtime.jsxs)("div", {
				className: HeaderLeadingControls_module_css_default.controls,
				children: [(0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.Tooltip, {
					label: t("toggle.open"),
					shortcutKeys: shortcut?.keys,
					delayMs: 500,
					children: (0, react_jsx_runtime.jsx)("button", {
						"data-sidebar-opener": true,
						type: "button",
						className: HeaderLeadingControls_module_css_default.iconButton,
						"aria-label": t("toggle.open"),
						"aria-keyshortcuts": shortcut?.aria,
						onClick: () => {
							toggleSidebar();
						},
						children: (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.IconPanelLeftOutlineRegular, { size: 16 })
					})
				}), desktop && canStartSession && (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.Tooltip, {
					label: t("session.new.label"),
					shortcutKeys: newShortcut?.keys,
					delayMs: 500,
					children: (0, react_jsx_runtime.jsx)("button", {
						type: "button",
						className: HeaderLeadingControls_module_css_default.iconButton,
						"aria-label": t("session.new.label"),
						"aria-keyshortcuts": newShortcut?.aria,
						onClick: () => {
							trackEntryClick({
								menu_name: "new_session",
								position: "titlebar"
							});
							startSession();
						},
						children: (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.IconNewChatOutlineRegular, { size: 16 })
					})
				})]
			});
		}
		//#endregion
		//#region ../../../node_modules/.pnpm/clsx@2.1.1/node_modules/clsx/dist/clsx.mjs
		function r(e) {
			var t, f, n = "";
			if ("string" == typeof e || "number" == typeof e) n += e;
			else if ("object" == typeof e) if (Array.isArray(e)) {
				var o = e.length;
				for (t = 0; t < o; t++) e[t] && (f = r(e[t])) && (n && (n += " "), n += f);
			} else for (f in e) e[f] && (n && (n += " "), n += f);
			return n;
		}
		function clsx() {
			for (var e, t, f = 0, n = "", o = arguments.length; f < o; f++) (e = arguments[f]) && (t = r(e)) && (n && (n += " "), n += t);
			return n;
		}
		//#endregion
		//#region lib/types/client/scroll-handoff.js
		/**
		* Wheel events closer together than this belong to one gesture, including its momentum phase.
		* DOM wheel events carry no gesture phase, so the gap stands in for it.
		*/
		const GESTURE_GAP_MS = 100;
		/**
		* Vertical wheel distance in pixels. Pinch-zoom and mostly horizontal events return undefined
		* and keep native scrolling.
		*/
		function verticalWheelPixels(event, page) {
			if (event.ctrlKey || Math.abs(event.deltaX) > Math.abs(event.deltaY)) return void 0;
			if (event.deltaMode === WheelEvent.DOM_DELTA_LINE) return event.deltaY * 16;
			if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) return event.deltaY * page;
			return event.deltaY;
		}
		/** Scroll by `delta`; the browser clamps the write, and the moved distance is returned. */
		function scrollClamped(element, delta) {
			const before = element.scrollTop;
			element.scrollTop = before + delta;
			return element.scrollTop - before;
		}
		/** Whether the viewport has scrolled the navigation away; sub-pixel remainders count as the end. */
		function atEnd(viewport) {
			return viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop < 1;
		}
		/**
		* Create the hand-off for one sidebar render tree.
		* @returns The viewport ref callback and the occupant hook; both keep their identity.
		*/
		function createScrollHandoff() {
			let scroller = null;
			let releaseViewport;
			let lastSplit = Number.NEGATIVE_INFINITY;
			const handle = (viewport, event) => {
				if (scroller === null || event.defaultPrevented) return;
				const delta = verticalWheelPixels(event, viewport.clientHeight);
				if (delta === void 0 || delta === 0) return;
				if (!(event.cancelable && event.timeStamp - lastSplit < GESTURE_GAP_MS)) {
					const native = atEnd(viewport) && scroller.contains(event.target);
					if (native && (delta > 0 || scroller.scrollTop + delta >= 0)) return;
					if (!event.cancelable) {
						if (native) scrollClamped(viewport, delta + scroller.scrollTop);
						return;
					}
				}
				event.preventDefault();
				lastSplit = event.timeStamp;
				if (delta > 0) {
					const rest = delta - scrollClamped(viewport, delta);
					if (rest > 0) scrollClamped(scroller, rest);
					return;
				}
				const rest = delta - scrollClamped(scroller, delta);
				if (rest < 0) scrollClamped(viewport, rest);
			};
			const sectionScroller = { bindScrollerRef: (element) => {
				scroller = element;
			} };
			return {
				bindViewportRef: (element) => {
					releaseViewport?.();
					releaseViewport = void 0;
					if (element === null) return;
					const onWheel = (event) => {
						handle(element, event);
					};
					element.addEventListener("wheel", onWheel, { passive: false });
					releaseViewport = () => {
						element.removeEventListener("wheel", onWheel);
					};
				},
				useSectionScroller: () => sectionScroller
			};
		}
		//#endregion
		//#region \0cds-css:/upstream/packages/client/ui-sidebar/src/client/SidebarRoot.module.css.mjs
		const css = ".s79m1a_root{--cds-sidebar-inline-padding:8px;--cds-sidebar-row-padding:12px;--cds-sidebar-label-gap:6px;--cds-sidebar-paint-inset:3px;height:100%;padding:6px var(--cds-sidebar-inline-padding);box-sizing:border-box;background:var(--dsw-specific-sidebar-fill);color:var(--dsw-alias-label-primary);--cds-scrollbar-thumb:var(--dsw-alias-scrollbar-bg-l2);--cds-scrollbar-thumb-hover:var(--dsw-alias-scrollbar-hover-l2);flex-direction:column;font-size:14px;display:flex}[data-animating]:not([data-dragging]) .s79m1a_root{transition:width var(--ds-transition-duration-slow) var(--ds-ease-in-out)}[data-platform=darwin] .s79m1a_root{background:0 0}.s79m1a_root.s79m1a_collapsed{padding:18px 10px 6px}[data-windows-titlebar] .s79m1a_root .s79m1a_logoRow{height:40px;margin:0;padding:0}[data-windows-titlebar] .s79m1a_toggle{top:calc((var(--cds-windows-titlebar-height) - 28px) / 2);z-index:30;-webkit-app-region:no-drag;position:fixed;left:12px}[data-windows-titlebar] .s79m1a_root.s79m1a_collapsed .s79m1a_logoRow{height:0}[data-windows-titlebar] .s79m1a_root:not(.s79m1a_collapsed) .s79m1a_brand{padding-left:8px}[data-windows-titlebar] .s79m1a_brandIdentity{transform:translateY(1px)}[data-windows-titlebar] .s79m1a_brandMark{transform:translate(1px)}[data-windows-titlebar] .s79m1a_brandName{font-weight:400}[data-windows-titlebar] .s79m1a_root.s79m1a_collapsed{padding:0}[data-windows-titlebar] .s79m1a_root:not(.s79m1a_collapsed) .s79m1a_newSession{margin-top:6px}[data-windows-titlebar] .s79m1a_collapsed .s79m1a_panelList,[data-windows-titlebar] .s79m1a_collapsed .s79m1a_regionArea,[data-windows-titlebar] .s79m1a_collapsed .s79m1a_footArea{display:none}html[data-windows-titlebar]:has([data-sidebar-collapsed=true]){--cds-windows-menu-start:84px}[data-windows-titlebar] .s79m1a_collapsed .s79m1a_newSession{top:calc((var(--cds-windows-titlebar-height) - 28px) / 2);z-index:30;-webkit-app-region:no-drag;border:none;margin:0;padding:0;position:fixed;left:48px}[data-windows-titlebar] .s79m1a_railIn .s79m1a_iconButton,[data-windows-titlebar] .s79m1a_railIn .s79m1a_newSession{animation:none}[data-windows-titlebar] .s79m1a_collapsed .s79m1a_toggle,[data-windows-titlebar] .s79m1a_collapsed .s79m1a_newSession{border-radius:var(--dsw-radius-sm);width:28px;height:28px;color:var(--dsw-alias-label-secondary)}[data-windows-titlebar] .s79m1a_collapsed .s79m1a_toggle .s79m1a_panelIcon{display:inline}.s79m1a_root.s79m1a_quietBars{--cds-scrollbar-thumb:transparent;--cds-scrollbar-thumb-hover:transparent}.s79m1a_fading>*{opacity:0;transition:opacity .15s var(--ds-ease-in-out)}.s79m1a_wide{animation:s79m1a_wide-in .2s var(--ds-ease-in-out)}@keyframes s79m1a_wide-in{0%{opacity:0}}.s79m1a_railIn .s79m1a_iconButton,.s79m1a_railIn .s79m1a_newSession,.s79m1a_railIn .s79m1a_panelList,.s79m1a_railIn .s79m1a_regionArea{animation:s79m1a_rail-in .15s var(--ds-ease-in-out) backwards}.s79m1a_railIn .s79m1a_footArea{animation:s79m1a_rail-fade-in .15s var(--ds-ease-in-out) backwards}@keyframes s79m1a_rail-in{0%{opacity:0;transform:translate(49px)}}@keyframes s79m1a_rail-fade-in{0%{opacity:0}}.s79m1a_topStrip{box-sizing:border-box;height:52px;margin:-6px calc(-1 * var(--cds-sidebar-inline-padding)) 0;flex:none;justify-content:flex-end;align-items:center;padding:0 12px 2px;display:flex}html[data-platform=darwin]:not([data-fullscreen]) .s79m1a_root:not(.s79m1a_collapsed)>.s79m1a_topStrip>.s79m1a_toggle{transform:translate(4px)}[data-fullscreen] .s79m1a_topStrip{justify-content:flex-start}.s79m1a_topStrip+.s79m1a_logoRow{margin-top:-12px}.s79m1a_logoRow{box-sizing:border-box;flex:none;justify-content:flex-end;align-items:center;gap:8px;height:60px;margin-bottom:0;padding:8px 0 8px 8px;display:flex;overflow:hidden}.s79m1a_collapsed .s79m1a_logoRow{justify-content:flex-start;height:36px;margin-bottom:12px;padding:0;overflow:visible}.s79m1a_brand{min-width:0;color:inherit;cursor:pointer;background:0 0;border:none;flex:1;align-items:center;padding:0;display:inline-flex;overflow:hidden}[data-platform=darwin] .s79m1a_brand{cursor:default}.s79m1a_brandIdentity{align-items:center;gap:5px;min-width:0;height:24px;display:inline-flex}.s79m1a_brandMark{flex:none;justify-content:center;align-items:center;display:inline-flex}.s79m1a_brandName{letter-spacing:.04em;align-items:center;gap:6px;min-width:0;height:24px;font-size:18px;font-weight:600;line-height:24px;display:inline-flex}.s79m1a_fallbackBrandName{letter-spacing:0;white-space:nowrap;font-size:17px}.s79m1a_localBuildBrand{white-space:nowrap;flex-direction:column;flex:none;justify-content:center;align-items:flex-start;gap:1px;height:24px;display:inline-flex}.s79m1a_copyVersion{cursor:pointer;-webkit-app-region:no-drag;display:inline-flex}.s79m1a_localBuildTitle{letter-spacing:0;font-size:12px;line-height:13px}.s79m1a_iconButton{border-radius:var(--dsw-radius-sm);cursor:pointer;width:28px;height:28px;color:var(--dsw-alias-label-secondary);background:0 0;border:none;flex:none;justify-content:center;align-items:center;padding:0;display:inline-flex;position:relative}@media (hover:hover){.s79m1a_iconButton:hover{background-color:var(--dsw-alias-interactive-bg-hover)}}.s79m1a_iconButton.s79m1a_toggle:focus-visible{outline:var(--dsw-focus-ring-width) solid var(--dsw-focus-ring-color,var(--dsw-alias-state-business-primary));outline-offset:-2px}.s79m1a_collapsed .s79m1a_iconButton{border-radius:var(--dsw-radius-md);width:36px;height:36px}.s79m1a_collapsed .s79m1a_toggle .s79m1a_panelIcon{display:none}@media (hover:hover){.s79m1a_collapsed .s79m1a_toggle:hover .s79m1a_panelIcon{display:inline}.s79m1a_collapsed .s79m1a_toggle:hover .s79m1a_railMark{display:none}}.s79m1a_railMark{justify-content:center;align-items:center;display:inline-flex}.s79m1a_collapsed .s79m1a_iconButton{color:var(--dsw-alias-label-primary)}.s79m1a_buildVersion{height:10px;color:var(--dsw-alias-label-primary-inverted);background:var(--dsw-alias-label-primary);font-family:var(--ds-font-family-code);white-space:nowrap;border-radius:2px;flex:none;align-items:center;padding:0 3px;font-size:6px;font-weight:500;line-height:10px;display:inline-flex}.s79m1a_newSession{height:38px;margin:0 calc(var(--cds-sidebar-paint-inset) + 1px);box-sizing:border-box;border:.5px solid var(--dsw-alias-border-l3);border-radius:var(--dsw-radius-md);background:var(--dsw-alias-button-elevated-fill);color:var(--dsw-alias-label-primary);cursor:pointer;flex:none;justify-content:center;align-items:center;gap:0;padding:8px 12px;font-size:14px;font-weight:500;line-height:22px;display:flex;position:relative;overflow:hidden;container-type:inline-size}@media (hover:hover){.s79m1a_newSession:hover{background:var(--dsw-alias-button-floating-hover)}}[data-platform=darwin] :where(.s79m1a_root:not([role=dialog])) .s79m1a_newSession{background:#ffffff8c}@media (hover:hover){[data-platform=darwin] :where(.s79m1a_root:not([role=dialog])) .s79m1a_newSession:hover{background:#ffffff4d}}[data-platform=darwin] [data-ds-dark-theme] :where(.s79m1a_root:not([role=dialog])) .s79m1a_newSession{background:#ffffff26}@media (hover:hover){[data-platform=darwin] [data-ds-dark-theme] :where(.s79m1a_root:not([role=dialog])) .s79m1a_newSession:hover{background:#fff3}}.s79m1a_collapsed .s79m1a_newSession{background:0 0;border-color:#0000;align-self:flex-start;gap:0;width:36px;height:36px;margin:0 0 12px;padding:0}@media (hover:hover){.s79m1a_collapsed .s79m1a_newSession:hover{background-color:var(--dsw-alias-interactive-bg-hover)}}.s79m1a_newSessionLabel{white-space:nowrap;max-width:200px;overflow:hidden}.s79m1a_collapsed .s79m1a_newSessionLabel{max-width:0}.s79m1a_newSessionLabelMask{flex:1;min-width:0}.s79m1a_newSession:focus-visible .s79m1a_newSessionLabelMask:has(+.s79m1a_newSessionShortcut){overflow:hidden;mask-image:linear-gradient(90deg,#000 calc(100% - 16px),#0000)}@media (hover:hover){.s79m1a_newSession:hover .s79m1a_newSessionLabelMask:has(+.s79m1a_newSessionShortcut){overflow:hidden;mask-image:linear-gradient(90deg,#000 calc(100% - 16px),#0000)}}.s79m1a_newSessionContent{justify-content:center;align-items:center;gap:6px;width:100cqw;display:flex}.s79m1a_newSessionContent>svg{flex:none}.s79m1a_newSessionShortcut{opacity:0;pointer-events:none;flex:none;font-weight:400;display:inline-flex}.s79m1a_newSession:focus-visible .s79m1a_newSessionShortcut{opacity:1}@media (hover:hover){.s79m1a_newSession:hover .s79m1a_newSessionShortcut{opacity:1}}.s79m1a_scrollRegion{flex-direction:column;flex:1;min-height:0;margin-top:4px;display:flex;position:relative}.s79m1a_root:not(.s79m1a_collapsed) .s79m1a_scrollRegion{container-type:size}.s79m1a_scrollArea{grid-template-columns:4px minmax(0, 1fr) calc(var(--cds-sidebar-inline-padding) - 2px);min-height:0;margin-left:-4px;margin-right:calc(2px - var(--cds-sidebar-inline-padding));flex:1;grid-template-rows:8px max-content 2px max-content;align-content:start;display:grid;overflow:hidden auto}.s79m1a_root:not(.s79m1a_collapsed) .s79m1a_scrollArea{scrollbar-width:none;container-type:scroll-state}.s79m1a_root:not(.s79m1a_collapsed) .s79m1a_scrollArea::-webkit-scrollbar{display:none}.s79m1a_scrollArea:before,.s79m1a_scrollArea:after{content:\"\";pointer-events:none;grid-column:2}.s79m1a_scrollArea:before{grid-row:1}.s79m1a_scrollArea:after{grid-row:3}.s79m1a_scrollFade{inset-inline:0;background:linear-gradient(to bottom, transparent, var(--dsw-specific-sidebar-fill));pointer-events:none;height:24px;position:absolute;bottom:0}[data-platform=darwin] .s79m1a_scrollFade,.s79m1a_collapsed .s79m1a_scrollFade,.s79m1a_collapsed .s79m1a_scrollArea:before,.s79m1a_collapsed .s79m1a_scrollArea:after{display:none}.s79m1a_collapsed .s79m1a_scrollRegion{margin-top:0}.s79m1a_collapsed .s79m1a_scrollArea{flex-direction:column;margin:0;padding:0;display:flex;overflow:visible}.s79m1a_panelEntry{border-inline:var(--cds-sidebar-paint-inset) solid transparent;border-radius:var(--dsw-radius-md);background-clip:padding-box;display:flex;position:relative}.s79m1a_panelEntryWide>:first-child{flex:1;min-width:0}.s79m1a_panelEntryWide .s79m1a_panelRow{background:0 0;border-inline:0;width:100%;margin:0;padding-right:38px}.s79m1a_panelEntryActive,.s79m1a_panelEntry:has(:focus-visible){background-color:var(--dsw-alias-interactive-bg-hover)}.s79m1a_panelUnpin{border-radius:var(--dsw-radius-sm);width:28px;height:28px;color:var(--dsw-alias-label-caption);cursor:pointer;background:0 0;border:0;justify-content:center;align-items:center;padding:0;display:inline-flex;position:absolute;top:2px;right:0}.s79m1a_panelUnpin:focus-visible{outline:var(--dsw-focus-ring-width) solid var(--dsw-focus-ring-color,var(--dsw-alias-state-business-primary));outline-offset:-2px}@media (hover:hover){.s79m1a_panelUnpin{opacity:0}.s79m1a_panelEntry:hover{background-color:var(--dsw-alias-interactive-bg-hover)}.s79m1a_panelEntry .s79m1a_panelRow:hover{background:0 0}.s79m1a_panelUnpin:hover{color:var(--dsw-alias-label-primary)}.s79m1a_panelEntry:hover .s79m1a_panelUnpin,.s79m1a_panelEntry:has(:focus-visible) .s79m1a_panelUnpin{opacity:1}}.s79m1a_panelList{flex-direction:column;flex:none;grid-area:2/2;gap:2px;margin-bottom:8px;display:flex}.s79m1a_panelRow{align-items:center;gap:var(--cds-sidebar-label-gap);min-height:32px;padding:5px calc(var(--cds-sidebar-row-padding) - var(--cds-sidebar-paint-inset));box-sizing:border-box;border:none;border-inline:var(--cds-sidebar-paint-inset) solid transparent;border-radius:var(--dsw-radius-md);color:var(--dsw-alias-label-primary);font:inherit;text-align:left;cursor:pointer;background:padding-box padding-box;margin:0;line-height:22px;display:flex}@media (hover:hover){.s79m1a_panelRow:hover{background-color:var(--dsw-alias-interactive-bg-hover)}}.s79m1a_panelRow:focus-visible{outline:var(--dsw-focus-ring-width) solid var(--dsw-focus-ring-color,var(--dsw-alias-state-business-primary));outline-offset:-2px}.s79m1a_moreAnchor{flex-direction:column;display:flex}.s79m1a_panelRow[aria-expanded=true]{background-color:var(--dsw-alias-interactive-bg-hover)}.s79m1a_moreMenu{width:288px;max-width:calc(100vw - 24px)}.s79m1a_panelGlyph{flex:none;justify-content:center;align-items:center;display:inline-flex}.s79m1a_panelTitle{text-overflow:ellipsis;white-space:nowrap;min-width:0;overflow:hidden}.s79m1a_collapsed .s79m1a_panelList{gap:12px;margin-bottom:12px}.s79m1a_collapsed .s79m1a_panelEntry{border-inline:0}.s79m1a_collapsed .s79m1a_panelRow{width:36px;height:36px;color:var(--dsw-alias-label-primary);background-clip:border-box;border-inline:0;justify-content:center;margin:0;padding:0}.s79m1a_regionArea{flex-direction:column;grid-area:4/2;min-width:0;min-height:0;display:flex}.s79m1a_collapsed .s79m1a_regionArea{flex:1}.s79m1a_footArea{flex-direction:column;flex:none;display:flex}.s79m1a_settingsArea,.s79m1a_footerActions{flex:none;width:100%;min-width:0}.s79m1a_footerActions{display:flex}.s79m1a_collapsed .s79m1a_footArea{align-items:center}.s79m1a_collapsed .s79m1a_settingsArea,.s79m1a_collapsed .s79m1a_footerActions{justify-content:center;width:auto;display:flex}@media (prefers-reduced-motion:reduce){[data-animating]:not([data-dragging]) .s79m1a_root,.s79m1a_wide,.s79m1a_fading>*,.s79m1a_railIn .s79m1a_iconButton,.s79m1a_railIn .s79m1a_newSession,.s79m1a_railIn .s79m1a_panelList,.s79m1a_railIn .s79m1a_footArea,.s79m1a_railIn .s79m1a_regionArea{transition:none;animation:none}}";
		const tagId = "@codesmith/client-ui-sidebar/SidebarRoot.module.css";
		if (typeof document !== "undefined" && document.querySelector("style[data-plugin-css=" + JSON.stringify(tagId) + "]") === null) {
			const tag = document.createElement("style");
			tag.dataset.plugin = "@codesmith/client-ui-sidebar";
			tag.dataset.pluginCss = tagId;
			tag.textContent = css;
			document.head.appendChild(tag);
		}
		var SidebarRoot_module_css_default = {
			"brand": "s79m1a_brand",
			"brandIdentity": "s79m1a_brandIdentity",
			"brandMark": "s79m1a_brandMark",
			"brandName": "s79m1a_brandName",
			"buildVersion": "s79m1a_buildVersion",
			"collapsed": "s79m1a_collapsed",
			"copyVersion": "s79m1a_copyVersion",
			"fading": "s79m1a_fading",
			"fallbackBrandName": "s79m1a_fallbackBrandName",
			"footArea": "s79m1a_footArea",
			"footerActions": "s79m1a_footerActions",
			"iconButton": "s79m1a_iconButton",
			"localBuildBrand": "s79m1a_localBuildBrand",
			"localBuildTitle": "s79m1a_localBuildTitle",
			"logoRow": "s79m1a_logoRow",
			"moreAnchor": "s79m1a_moreAnchor",
			"moreMenu": "s79m1a_moreMenu",
			"newSession": "s79m1a_newSession",
			"newSessionContent": "s79m1a_newSessionContent",
			"newSessionLabel": "s79m1a_newSessionLabel",
			"newSessionLabelMask": "s79m1a_newSessionLabelMask",
			"newSessionShortcut": "s79m1a_newSessionShortcut",
			"panelEntry": "s79m1a_panelEntry",
			"panelEntryActive": "s79m1a_panelEntryActive",
			"panelEntryWide": "s79m1a_panelEntryWide",
			"panelGlyph": "s79m1a_panelGlyph",
			"panelIcon": "s79m1a_panelIcon",
			"panelList": "s79m1a_panelList",
			"panelRow": "s79m1a_panelRow",
			"panelTitle": "s79m1a_panelTitle",
			"panelUnpin": "s79m1a_panelUnpin",
			"quietBars": "s79m1a_quietBars",
			"rail-fade-in": "s79m1a_rail-fade-in",
			"rail-in": "s79m1a_rail-in",
			"railIn": "s79m1a_railIn",
			"railMark": "s79m1a_railMark",
			"regionArea": "s79m1a_regionArea",
			"root": "s79m1a_root",
			"scrollArea": "s79m1a_scrollArea",
			"scrollFade": "s79m1a_scrollFade",
			"scrollRegion": "s79m1a_scrollRegion",
			"settingsArea": "s79m1a_settingsArea",
			"toggle": "s79m1a_toggle",
			"topStrip": "s79m1a_topStrip",
			"wide": "s79m1a_wide",
			"wide-in": "s79m1a_wide-in"
		};
		//#endregion
		//#region lib/types/client/SidebarRoot.js
		/**
		* Sidebar shell: column geometry and global panel navigation.
		* Collapse is a slide plus crossfade:
		* content freezes at its expanded width (inline style) and fades out in place
		* while the sliding column (AppFrame grid tracks) clips it — nothing reflows
		* mid-slide. At settle the wide-only content unmounts and the upper
		* controls enter the 56px rail from the same horizontal offset (one icon each,
		* same top-down order) on one fade that ends with the slide. The bottom-pinned
		* settings control only fades. The workspace/session browsing region between
		* global panel rows and the foot is the `sidebar.workspaces` registrant's,
		* and the foot holds `sidebar.settings` plus `sidebar.footer.action`; the shell
		* hands them the wide flag (plus an expand request callback for the browser).
		* Through the browser's hookContext, the shell also hands it the scroller hook
		* that continues wheel scrolling over the sidebar viewport into its list.
		*
		* The column also owns whether the scroll regions nested in it draw a
		* scrollbar at all: the shell tracks the pointer and rebinds ui-theme's
		* scrollbar indirection away while it is elsewhere, so a list the user is not
		* pointing at carries no bar.
		*/
		/** Wide-content unmount delay; matches the 150ms wide-content fade-out. */
		const COLLAPSE_SETTLE_MS = 150;
		/**
		* How long the column's scrollbars stay drawn after the pointer leaves it.
		* The bar is a pointer affordance here, and hiding it on the leave event
		* itself makes it blink out while the pointer is only crossing the column's
		* edge — on the way to the conversation, or around a portalled menu.
		*/
		const SCROLLBAR_LINGER_MS = 2e3;
		/** Format complete-build metadata for the local brand badge. */
		function localBuildVersion() {
			return `0.2.2-alpha.3-8bd0733` + ({}.CODESMITH_CLIENT_GIT_DIRTY === "true" ? "-dirty" : "");
		}
		/** Analytics category of an official panel; other panels are never reported. */
		function officialEntry(id) {
			if (id === "plugins") return "plugin";
			return id === "schedules" ? "cron" : void 0;
		}
		/** Each panel row subscribes only to its own selection state. */
		function PanelRow({ id, label, wide, unpinLabel, unpinTooltip, canUnpin, usePanelInfo, selectPanel, setPinned, trackEntryClick, renderFactorySlot }) {
			const active = usePanelInfo((info) => info.activePanelId === id);
			return (0, react_jsx_runtime.jsxs)("div", {
				className: clsx(SidebarRoot_module_css_default.panelEntry, wide && SidebarRoot_module_css_default.panelEntryWide, active && SidebarRoot_module_css_default.panelEntryActive),
				children: [(0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.Tooltip, {
					label,
					delayMs: 500,
					disabled: wide,
					children: (0, react_jsx_runtime.jsxs)("button", {
						type: "button",
						className: SidebarRoot_module_css_default.panelRow,
						"aria-label": label,
						"aria-current": active ? "page" : void 0,
						onClick: () => {
							const entry = officialEntry(id);
							if (entry !== void 0) trackEntryClick({
								menu_name: entry,
								position: "sidebar"
							});
							selectPanel(id);
						},
						children: [(0, react_jsx_runtime.jsx)("span", {
							className: SidebarRoot_module_css_default.panelGlyph,
							"aria-hidden": "true",
							children: renderFactorySlot("sidebar.panel.icon", {
								id,
								size: wide ? 16 : 18,
								active
							})
						}), wide && (0, react_jsx_runtime.jsx)("span", {
							className: clsx(SidebarRoot_module_css_default.panelTitle, SidebarRoot_module_css_default.wide),
							children: label
						})]
					})
				}), wide && canUnpin && (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.Tooltip, {
					label: unpinTooltip,
					side: "bottom",
					align: "end",
					delayMs: 500,
					portal: true,
					children: (0, react_jsx_runtime.jsx)("button", {
						type: "button",
						className: SidebarRoot_module_css_default.panelUnpin,
						"aria-label": unpinLabel,
						onClick: (event) => {
							(0, _codesmith_client_ui_primitives.focusWithoutRing)(event.currentTarget.closest("nav").querySelector("button[aria-haspopup=\"menu\"]"));
							setPinned(id, false);
						},
						children: (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.IconPinFillRegular, { size: 14 })
					})
				})]
			});
		}
		/** The panel list's More launcher: a panel-row button whose menu rows come from `sidebar.panellist.actions`. */
		function PanelMoreMenu({ wide, label, entries, onOpen, onPinnedChange, trackEntryClick, renderSlot }) {
			const [open, setOpen] = (0, react.useState)(false);
			const onDismiss = () => {
				setOpen(false);
			};
			return (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.Menu, {
				open,
				onClose: onDismiss,
				portal: true,
				autoFocus: true,
				className: SidebarRoot_module_css_default.moreAnchor,
				listClassName: SidebarRoot_module_css_default.moreMenu,
				anchor: (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.Tooltip, {
					label,
					delayMs: 500,
					disabled: wide,
					children: (0, react_jsx_runtime.jsxs)("button", {
						type: "button",
						className: SidebarRoot_module_css_default.panelRow,
						"aria-label": label,
						"aria-haspopup": "menu",
						"aria-expanded": open,
						onClick: () => {
							trackEntryClick({
								menu_name: "more",
								position: "sidebar"
							});
							setOpen((value) => !value);
						},
						children: [(0, react_jsx_runtime.jsx)("span", {
							className: SidebarRoot_module_css_default.panelGlyph,
							"aria-hidden": "true",
							children: (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.IconAppsAddOutlineRegular, { size: wide ? 16 : 18 })
						}), wide && (0, react_jsx_runtime.jsx)("span", {
							className: clsx(SidebarRoot_module_css_default.panelTitle, SidebarRoot_module_css_default.wide),
							children: label
						})]
					})
				}),
				children: renderSlot("sidebar.panellist.actions", {
					entries,
					onOpen,
					onPinnedChange,
					onDismiss
				})
			});
		}
		/**
		* Render the sidebar column shell.
		* @param props - composed slot props (runtime share + injected callbacks, contract/slots.ts).
		* @returns the sidebar element tree.
		*/
		function SidebarRoot({ collapsed, width, drawer, startSession, toggleSidebar, selectPanel, setPinned, trackEntryClick, useRemoteAccess, usePanels, useEntryPins, useEntryActions, useShortcuts, usePanelInfo, t, renderSlot, renderFactorySlot }) {
			const panels = usePanels((snapshot) => snapshot);
			const unpinned = useEntryPins((ids) => ids);
			const activePanelId = usePanelInfo((info) => info.activePanelId);
			const hasEntryActions = useEntryActions((registered) => registered);
			const shortcut = useShortcuts((rows) => rows.find((row) => row.id === "sidebar.left.toggle"));
			const newShortcut = useShortcuts((rows) => rows.find((row) => row.id === "session.new"));
			const canStartSession = useRemoteAccess("session/create");
			const toggleLabel = collapsed ? t("toggle.open") : t("toggle.collapse");
			const [settled, setSettled] = (0, react.useState)(collapsed);
			(0, react.useEffect)(() => {
				if (drawer || !collapsed) {
					setSettled(collapsed);
					return;
				}
				const timer = window.setTimeout(() => {
					setSettled(true);
				}, COLLAPSE_SETTLE_MS);
				return () => {
					window.clearTimeout(timer);
				};
			}, [collapsed, drawer]);
			const windowsTitlebar = document.documentElement.hasAttribute("data-windows-titlebar");
			const wide = drawer || (windowsTitlebar ? !collapsed : !collapsed || !settled);
			const [scrollHandoff] = (0, react.useState)(createScrollHandoff);
			const sectionHookContext = (0, react.useMemo)(() => ({ useSectionScroller: scrollHandoff.useSectionScroller }), [scrollHandoff]);
			const openNewSession = () => {
				trackEntryClick({
					menu_name: "new_session",
					position: windowsTitlebar && collapsed ? "titlebar" : "sidebar"
				});
				startSession();
			};
			const captionTooltipSide = windowsTitlebar ? "bottom" : "right";
			const lastWideWidth = (0, react.useRef)(width);
			if (drawer || !collapsed) lastWideWidth.current = width;
			const animateRail = (0, react.useRef)(!collapsed);
			if (drawer || !collapsed) animateRail.current = !collapsed;
			const column = (0, react.useRef)(null);
			const sidebarFocus = (0, react.useRef)(null);
			(0, _codesmith_client_ui_primitives.useModalLayer)(column, drawer && !collapsed, toggleSidebar);
			(0, react.useLayoutEffect)(() => {
				if (!collapsed) return;
				const ownsMenuFocus = (sidebarFocus.current?.getAttribute("aria-controls")?.split(/\s+/) ?? []).some((id) => document.getElementById(id)?.contains(document.activeElement));
				if (sidebarFocus.current !== document.activeElement && !ownsMenuFocus) return;
				const opener = drawer ? column.current?.ownerDocument.querySelector("[data-sidebar-opener]") : column.current?.querySelector("[data-sidebar-toggle]");
				if (opener != null && !(0, _codesmith_client_ui_primitives.isBehindModal)(opener)) (0, _codesmith_client_ui_primitives.focusWithoutRing)(opener, { preventScroll: true });
			}, [drawer, collapsed]);
			const [pointerInside, setPointerInside] = (0, react.useState)(false);
			const lingerTimer = (0, react.useRef)(void 0);
			const armLinger = () => {
				if (lingerTimer.current !== void 0) return;
				lingerTimer.current = window.setTimeout(() => {
					lingerTimer.current = void 0;
					setPointerInside(false);
				}, SCROLLBAR_LINGER_MS);
			};
			const cancelLinger = () => {
				window.clearTimeout(lingerTimer.current);
				lingerTimer.current = void 0;
			};
			(0, react.useEffect)(() => {
				if (!pointerInside) return;
				const onMove = (event) => {
					const rect = column.current?.getBoundingClientRect();
					/* v8 ignore next -- the listener only exists while the column is mounted and revealed. */
					if (rect === void 0) return;
					if (event.clientX >= rect.left && event.clientX < rect.right && event.clientY >= rect.top && event.clientY < rect.bottom) cancelLinger();
					else armLinger();
				};
				document.addEventListener("pointermove", onMove);
				return () => {
					document.removeEventListener("pointermove", onMove);
					cancelLinger();
				};
			}, [pointerInside]);
			const buildVersion = localBuildVersion();
			const [copiedToast, setCopiedToast] = (0, react.useState)(null);
			const copyVersion = (version) => () => {
				(0, _codesmith_client_ui_primitives.writeClipboard)(version).then((ok) => {
					if (ok) setCopiedToast((seq) => (seq ?? 0) + 1);
				});
			};
			const darwinDesktop = (0, _codesmith_client_ui_primitives.isDarwinDesktop)();
			const toggle = (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.Tooltip, {
				label: toggleLabel,
				shortcutKeys: shortcut?.keys,
				delayMs: 500,
				side: captionTooltipSide,
				portal: drawer,
				children: (0, react_jsx_runtime.jsxs)("button", {
					"data-sidebar-toggle": true,
					"data-modal-autofocus": drawer || void 0,
					type: "button",
					className: clsx(SidebarRoot_module_css_default.iconButton, SidebarRoot_module_css_default.toggle),
					"aria-label": toggleLabel,
					"aria-keyshortcuts": shortcut?.aria,
					onClick: () => {
						toggleSidebar();
					},
					children: [
						!wide && !windowsTitlebar && (0, react_jsx_runtime.jsx)("span", {
							className: SidebarRoot_module_css_default.railMark,
							"aria-hidden": "true",
							children: renderSlot("sidebar.brand.mark", { size: 24 }, { fallback: (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.FishLogo, { size: 24 }) })
						}),
						(0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.IconPanelLeftOutlineRegular, {
							className: SidebarRoot_module_css_default.panelIcon,
							size: wide || windowsTitlebar ? 16 : 18
						}),
						!wide && renderSlot("sidebar.toggle.badge", {})
					]
				})
			});
			return (0, react_jsx_runtime.jsxs)("div", {
				ref: column,
				onFocusCapture: (event) => {
					sidebarFocus.current = event.target;
				},
				role: drawer ? "dialog" : void 0,
				"data-shortcut-modal": drawer ? "sidebar" : void 0,
				"aria-modal": drawer && !collapsed ? true : void 0,
				"aria-label": drawer ? t("navigation.label") : void 0,
				tabIndex: drawer ? -1 : void 0,
				className: clsx(SidebarRoot_module_css_default.root, !wide && SidebarRoot_module_css_default.collapsed, !wide && animateRail.current && SidebarRoot_module_css_default.railIn, !drawer && collapsed && wide && SidebarRoot_module_css_default.fading, !pointerInside && SidebarRoot_module_css_default.quietBars),
				style: wide ? { width: !drawer && collapsed ? lastWideWidth.current : width } : void 0,
				onPointerEnter: () => {
					cancelLinger();
					setPointerInside(true);
				},
				onPointerLeave: () => {
					armLinger();
				},
				children: [
					darwinDesktop && (0, react_jsx_runtime.jsx)("div", {
						className: SidebarRoot_module_css_default.topStrip,
						"data-window-drag": true,
						children: toggle
					}),
					(0, react_jsx_runtime.jsxs)("div", {
						className: SidebarRoot_module_css_default.logoRow,
						"data-window-drag": true,
						children: [wide && (() => {
							const identity = (0, react_jsx_runtime.jsxs)("span", {
								className: SidebarRoot_module_css_default.brandIdentity,
								"aria-hidden": "true",
								children: [(0, react_jsx_runtime.jsx)("span", {
									className: SidebarRoot_module_css_default.brandMark,
									children: renderSlot("sidebar.brand.mark", { size: 24 }, { fallback: buildVersion === void 0 ? (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.FishLogo, { size: 24 }) : (0, react_jsx_runtime.jsx)("span", {
										className: SidebarRoot_module_css_default.copyVersion,
										"data-copy-version": true,
										onClick: copyVersion(buildVersion),
										children: (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.FishLogo, { size: 24 })
									}) })
								}), (0, react_jsx_runtime.jsx)("span", {
									className: SidebarRoot_module_css_default.brandName,
									children: renderSlot("sidebar.brand.name", {}, { fallback: buildVersion === void 0 ? (0, react_jsx_runtime.jsx)("span", {
										className: SidebarRoot_module_css_default.fallbackBrandName,
										children: t("brand.localBuild")
									}) : (0, react_jsx_runtime.jsxs)("span", {
										className: clsx(SidebarRoot_module_css_default.localBuildBrand, SidebarRoot_module_css_default.copyVersion),
										"data-copy-version": true,
										onClick: copyVersion(buildVersion),
										children: [(0, react_jsx_runtime.jsx)("span", {
											className: SidebarRoot_module_css_default.localBuildTitle,
											children: t("brand.localBuild")
										}), (0, react_jsx_runtime.jsx)("span", {
											className: SidebarRoot_module_css_default.buildVersion,
											children: buildVersion
										})]
									}) })
								})]
							});
							return darwinDesktop || !canStartSession ? (0, react_jsx_runtime.jsx)("span", {
								className: clsx(SidebarRoot_module_css_default.brand, SidebarRoot_module_css_default.wide),
								children: identity
							}) : (0, react_jsx_runtime.jsx)("button", {
								type: "button",
								className: clsx(SidebarRoot_module_css_default.brand, SidebarRoot_module_css_default.wide),
								"aria-label": t("session.new.label"),
								"aria-keyshortcuts": newShortcut?.aria,
								onClick: (event) => {
									if (event.target.closest("[data-copy-version]") !== null) return;
									openNewSession();
								},
								children: identity
							});
						})(), !darwinDesktop && toggle]
					}),
					canStartSession && (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.Tooltip, {
						label: t("session.new.label"),
						shortcutKeys: newShortcut?.keys,
						delayMs: 500,
						side: captionTooltipSide,
						disabled: wide,
						children: (0, react_jsx_runtime.jsxs)("button", {
							type: "button",
							className: SidebarRoot_module_css_default.newSession,
							"aria-label": t("session.new.label"),
							"aria-keyshortcuts": newShortcut?.aria,
							onClick: openNewSession,
							children: [(0, react_jsx_runtime.jsx)("span", {
								className: SidebarRoot_module_css_default.newSessionLabelMask,
								children: (0, react_jsx_runtime.jsxs)("span", {
									className: SidebarRoot_module_css_default.newSessionContent,
									children: [wide ? (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.IconNewChatOutlineMedium, { size: 14 }) : (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.IconNewChatOutlineRegular, { size: windowsTitlebar ? 16 : 18 }), wide && (0, react_jsx_runtime.jsx)("span", {
										className: clsx(SidebarRoot_module_css_default.newSessionLabel, SidebarRoot_module_css_default.wide),
										children: t("session.new")
									})]
								})
							}), wide && newShortcut !== void 0 && newShortcut.keys.length > 0 && (0, react_jsx_runtime.jsx)("span", {
								className: SidebarRoot_module_css_default.newSessionShortcut,
								"aria-hidden": "true",
								children: (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.ShortcutKeys, { keys: newShortcut.keys })
							})]
						})
					}),
					(0, react_jsx_runtime.jsxs)("div", {
						className: SidebarRoot_module_css_default.scrollRegion,
						children: [(0, react_jsx_runtime.jsxs)("div", {
							ref: scrollHandoff.bindViewportRef,
							className: SidebarRoot_module_css_default.scrollArea,
							"data-sidebar-scrollport": wide ? "" : void 0,
							children: [panels.length > 0 && (0, react_jsx_runtime.jsxs)("nav", {
								className: SidebarRoot_module_css_default.panelList,
								"aria-label": t("panels.label"),
								children: [panels.filter(({ id }) => id === "plugins" || !hasEntryActions || !unpinned.includes(id)).map(({ id, label }) => (0, react_jsx_runtime.jsx)(PanelRow, {
									id,
									label,
									wide,
									unpinLabel: t("panels.unpin", { name: label }),
									unpinTooltip: t("panels.unpinTooltip"),
									canUnpin: hasEntryActions && id !== "plugins",
									setPinned,
									usePanelInfo,
									selectPanel,
									trackEntryClick,
									renderFactorySlot
								}, id)), hasEntryActions && (0, react_jsx_runtime.jsx)(PanelMoreMenu, {
									wide,
									label: t("panels.more"),
									entries: panels.filter(({ id }) => id !== "plugins").map(({ id, label }) => ({
										id,
										label,
										pinned: !unpinned.includes(id),
										active: id === activePanelId
									})),
									onOpen: (id) => {
										const entry = officialEntry(id);
										if (entry !== void 0) trackEntryClick({
											menu_name: entry,
											position: "sidebar_more"
										});
										const panel = panels.find((candidate) => candidate.id === id);
										if (panel !== void 0) selectPanel(panel.id);
									},
									onPinnedChange: setPinned,
									trackEntryClick,
									renderSlot
								})]
							}), (0, react_jsx_runtime.jsx)("div", {
								className: SidebarRoot_module_css_default.regionArea,
								children: renderSlot("sidebar.workspaces", {
									wide,
									expandSidebar: () => {
										if (collapsed) toggleSidebar();
									}
								}, { hookContext: sectionHookContext })
							})]
						}), (0, react_jsx_runtime.jsx)("span", {
							className: SidebarRoot_module_css_default.scrollFade,
							"aria-hidden": "true"
						})]
					}),
					copiedToast !== null && (0, react_jsx_runtime.jsx)(_codesmith_client_ui_primitives.Toast, {
						tone: "success",
						text: t("version.copied"),
						onDone: () => {
							setCopiedToast(null);
						}
					}, copiedToast),
					(0, react_jsx_runtime.jsxs)("div", {
						className: SidebarRoot_module_css_default.footArea,
						children: [(0, react_jsx_runtime.jsx)("div", {
							className: SidebarRoot_module_css_default.footerActions,
							children: renderSlot("sidebar.footer.action", { wide })
						}), (0, react_jsx_runtime.jsx)("div", {
							className: SidebarRoot_module_css_default.settingsArea,
							children: renderSlot("sidebar.settings", { wide })
						})]
					})
				]
			});
		}
		//#endregion
		//#region lib/types/client/PanelIcon.js
		/**
		* Render the icon registered for one panel at the caller's size and selection state.
		* @param props - Panel identity, icon presentation, and the authorized child renderer.
		* @returns The matching panel icon.
		*/
		function PanelIcon({ id, size, active, renderSlot }) {
			return renderSlot("sidebar.panellist", {
				size,
				active
			}, { only: id });
		}
		//#endregion
		//#region lib/types/client/locales.js
		/** `sidebar` namespace dictionaries for shell controls and global panels. */
		/** Simplified Chinese dictionary (the key-set source of truth). */
		const zh = {
			"panels.unpinTooltip": "从左侧栏取消固定",
			"panels.unpin": "从侧边栏取消固定{name}",
			"session.new": "新会话",
			"session.new.label": "新建会话",
			"toggle.open": "打开侧边栏",
			"toggle.collapse": "收起侧边栏",
			"panels.label": "全局面板",
			"navigation.label": "导航",
			"panels.more": "更多",
			"version.copied": "版本号复制成功"
		};
		/** English dictionary, checked complete against the zh key set. */
		const en = {
			"panels.unpinTooltip": "Unpin from left sidebar",
			"panels.unpin": "Unpin {name} from sidebar",
			"session.new": "New Session",
			"session.new.label": "New session",
			"toggle.open": "Open sidebar",
			"toggle.collapse": "Collapse sidebar",
			"panels.label": "Global panels",
			"navigation.label": "Navigation",
			"panels.more": "More",
			"version.copied": "Version copied"
		};
		/** Japanese copy, checked against the English keys. */
		const ja = {
			"panels.unpinTooltip": "左サイドバーから固定解除",
			"panels.unpin": "{name}をサイドバーから固定解除",
			"session.new": "新規セッション",
			"session.new.label": "新規セッション",
			"toggle.open": "サイドバーを開く",
			"toggle.collapse": "サイドバーを折りたたむ",
			"panels.label": "グローバルパネル",
			"navigation.label": "ナビゲーション",
			"panels.more": "その他",
			"version.copied": "バージョンをコピーしました"
		};
		/** Spanish dictionary, checked against the English keys. */
		const es = {
			"panels.unpinTooltip": "Desanclar de la barra lateral izquierda",
			"panels.unpin": "Desfijar {name} de la barra lateral",
			"session.new": "Nueva sesión",
			"session.new.label": "Nueva sesión",
			"toggle.open": "Abrir barra lateral",
			"toggle.collapse": "Contraer barra lateral",
			"panels.label": "Paneles globales",
			"navigation.label": "Navegación",
			"panels.more": "Más",
			"version.copied": "Versión copiada"
		};
		/** French dictionary, checked against the English keys. */
		const fr = {
			"panels.unpinTooltip": "Retirer l'épingle de la barre latérale gauche",
			"panels.unpin": "Désépingler {name} de la barre latérale",
			"session.new": "Nouvelle séance",
			"session.new.label": "Nouvelle séance",
			"toggle.open": "Ouvrir la barre latérale",
			"toggle.collapse": "Réduire la barre latérale",
			"panels.label": "Panneaux globaux",
			"navigation.label": "Navigation",
			"panels.more": "Plus",
			"version.copied": "Version copiée"
		};
		/** German dictionary, checked against the English keys. */
		const de = {
			"panels.unpinTooltip": "Von der linken Seitenleiste lösen",
			"panels.unpin": "{name} von der Seitenleiste lösen",
			"session.new": "Neue Sitzung",
			"session.new.label": "Neue Sitzung",
			"toggle.open": "Seitenleiste öffnen",
			"toggle.collapse": "Seitenleiste einklappen",
			"panels.label": "Globale Bereiche",
			"navigation.label": "Navigation",
			"panels.more": "Mehr",
			"version.copied": "Version kopiert"
		};
		/** Russian dictionary, checked against the English keys. */
		const ru = {
			"panels.unpinTooltip": "Открепить от левой боковой панели",
			"panels.unpin": "Открепить {name} от боковой панели",
			"session.new": "Новая сессия",
			"session.new.label": "Новая сессия",
			"toggle.open": "Открыть боковую панель",
			"toggle.collapse": "Свернуть боковую панель",
			"panels.label": "Глобальные панели",
			"navigation.label": "Навигация",
			"panels.more": "Ещё",
			"version.copied": "Версия скопирована"
		};
		/** Portuguese dictionary, checked against the English keys. */
		const pt = {
			"panels.unpinTooltip": "Soltar da barra lateral esquerda",
			"panels.unpin": "Desafixar {name} da barra lateral",
			"session.new": "Nova sessão",
			"session.new.label": "Nova sessão",
			"toggle.open": "Abrir barra lateral",
			"toggle.collapse": "Recolher barra lateral",
			"panels.label": "Painéis globais",
			"navigation.label": "Navegação",
			"panels.more": "Mais",
			"version.copied": "Versão copiada"
		};
		//#endregion
		//#region lib/types/client/index.js
		/** The section occupant receives the scroller hook from each render's hookContext. */
		const SIDEBAR_SECTION_INJECT = { hooks: { sectionScroller: (_standard, { useSectionScroller }) => useSectionScroller } };
		/** Dictionary namespace owned by this plugin. */
		const NS = "sidebar";
		/** Services required by the sidebar plugin. */
		const inject = [
			"slots",
			"layout",
			"uiWorkspace",
			"locale",
			"shortcuts"
		];
		/** Registers the sidebar shell and its service callbacks.
		* @param ctx - Client root context.
		*/
		function apply(ctx) {
			const workspaceNavigation = ctx.get("uiWorkspace");
			ctx.effect(() => ctx.locale.register(NS, {
				zh,
				en,
				ja,
				es,
				fr,
				de,
				ru,
				pt
			}), "ui-sidebar: dictionaries");
			const panels = (0, _codesmith_client_store.createSnapshotStore)([]);
			const syncPanels = () => {
				const next = ctx.slots.entriesOfSlot("sidebar.panellist").map(({ options }) => {
					const id = options.id;
					return {
						id,
						order: options.order ?? 0,
						label: (0, _codesmith_client_ui_slots.resolveSlotLabel)(options.label) ?? id
					};
				}).sort((a, b) => a.order - b.order);
				const previous = panels.getSnapshot();
				if (previous.length === next.length && previous.every((panel, index) => {
					const candidate = next[index];
					return panel.id === candidate.id && panel.order === candidate.order && panel.label === candidate.label;
				})) return;
				panels.set(next);
			};
			ctx.effect(() => ctx.slots.subscribe("sidebar.panellist", syncPanels), "ui-sidebar: panel entries");
			ctx.effect(() => ctx.locale.subscribe(syncPanels), "ui-sidebar: panel labels");
			const panelPins = createPanelPins();
			const entryActions = {
				getSnapshot: () => ctx.slots.entriesOfSlot("sidebar.panellist.actions").length > 0,
				subscribe: (listener) => ctx.slots.subscribe("sidebar.panellist.actions", listener)
			};
			const injectProps = () => ({
				startSession: (workspaceId) => {
					workspaceNavigation.startSession(workspaceId);
				},
				toggleSidebar: () => {
					ctx.layout.toggleSidebar();
				},
				selectPanel: (id) => {
					ctx.layout.selectPanel(id);
				},
				setPinned: panelPins.setPinned,
				trackEntryClick: (attributes) => {
					ctx.get("productAnalytics")?.track("sidebar_menu_click", attributes);
				},
				hooks: {
					panels,
					shortcuts: ctx.shortcuts.catalog,
					entryPins: panelPins.unpinned,
					entryActions
				}
			});
			ctx.slots.inject("sidebar", function* () {
				yield ctx.slots.registerFactory({
					name: "sidebar.panel.icon",
					scope: "root",
					children: { "sidebar.panellist": {
						kind: "list",
						scope: "root"
					} }
				}, PanelIcon);
				yield ctx.slots.register({
					name: "sidebar",
					locale: NS,
					children: {
						"sidebar.brand.mark": {
							kind: "single",
							scope: "root"
						},
						"sidebar.brand.name": {
							kind: "single",
							scope: "root"
						},
						"sidebar.toggle.badge": {
							kind: "single",
							scope: "root"
						},
						"sidebar.panellist.actions": {
							kind: "list",
							scope: "root"
						},
						"sidebar.workspaces": {
							kind: "single",
							scope: "root",
							inject: SIDEBAR_SECTION_INJECT
						},
						"sidebar.settings": {
							kind: "single",
							scope: "root"
						},
						"sidebar.footer.action": {
							kind: "list",
							scope: "root"
						}
					},
					inject: injectProps
				}, SidebarRoot);
			});
			ctx.slots.inject("shell.leading", () => ctx.slots.register({
				name: "shell.leading",
				locale: NS,
				inject: injectProps
			}, HeaderLeadingControls));
			syncPanels();
		}
		//#endregion
		exports.apply = apply;
		exports.inject = inject;
		return module.exports;
	}
});

//# sourceMappingURL=client.js.map
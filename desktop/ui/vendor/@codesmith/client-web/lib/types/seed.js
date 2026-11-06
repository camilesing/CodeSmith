/**
 * Platform-singleton module-table. These are the ONLY entities the shell
 * shares into the frozen module table — fetch bundles resolve their externals
 * against exactly this set through the loader's require. Keys come from the
 * platform constant module ({@link ./platform.ts}, the single source
 * of truth with the tsdown client externals); values stay shell-static
 * imports so every bundle sees the same instance.
 */
import * as React from 'react';
import * as ReactJsxRuntime from 'react/jsx-runtime';
import * as ReactDom from 'react-dom';
import * as ReactDomClient from 'react-dom/client';
import * as Cordis from '@codesmith/cordis';
import * as Zod from 'zod';
import * as ReactVirtual from '@tanstack/react-virtual';
import * as ClientStore from '@codesmith/client-store';
import * as UiSlots from '@codesmith/client-ui-slots';
import * as UiPrimitives from '@codesmith/client-ui-primitives';
import * as UiDockkit from '@codesmith/client-ui-dockkit';
/**
 * Build the static table handed to the module loader at boot.
 * @returns module specifier → exported entity (one entry per platform word).
 */
export function getStaticModules() {
    // The satisfies pin is the projection contract: a word added to
    // PLATFORM_MODULES without a static import here (or vice versa) fails to
    // compile instead of drifting into a runtime require miss.
    return {
        'react': React,
        'react/jsx-runtime': ReactJsxRuntime,
        'react-dom': ReactDom,
        'react-dom/client': ReactDomClient,
        '@codesmith/cordis': Cordis,
        'zod': Zod,
        '@tanstack/react-virtual': ReactVirtual,
        '@codesmith/client-store': ClientStore,
        '@codesmith/client-ui-slots': UiSlots,
        '@codesmith/client-ui-primitives': UiPrimitives,
        '@codesmith/client-ui-dockkit': UiDockkit,
    };
}
//# sourceMappingURL=seed.js.map
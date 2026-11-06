/**
 * Shared browser platform modules. Seeding, bundling externals, and Vite
 * aliases consume this list so their module identities cannot drift.
 * @module @codesmith/client-web/src/platform
 */
/** The module specifiers the shell shares into the frozen module table. */
export declare const PLATFORM_MODULES: readonly ["react", "react/jsx-runtime", "react-dom", "react-dom/client", "@codesmith/cordis", "zod", "@tanstack/react-virtual", "@codesmith/client-store", "@codesmith/client-ui-slots", "@codesmith/client-ui-primitives", "@codesmith/client-ui-dockkit"];
/** Client-bundle specifiers whose factories the parser preloads before the shell starts. */
export declare const PRELOADED_CLIENT_EXTERNALS: readonly [];
/** One platform module specifier (a seed-table key). */
export type PlatformModule = (typeof PLATFORM_MODULES)[number];
//# sourceMappingURL=platform.d.ts.map

// this file is generated — do not edit it


/// <reference types="@sveltejs/kit" />

/**
 * This module provides access to environment variables that are injected _statically_ into your bundle at build time and are limited to _private_ access.
 * 
 * |         | Runtime                                                                    | Build time                                                               |
 * | ------- | -------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
 * | Private | [`$env/dynamic/private`](https://svelte.dev/docs/kit/$env-dynamic-private) | [`$env/static/private`](https://svelte.dev/docs/kit/$env-static-private) |
 * | Public  | [`$env/dynamic/public`](https://svelte.dev/docs/kit/$env-dynamic-public)   | [`$env/static/public`](https://svelte.dev/docs/kit/$env-static-public)   |
 * 
 * Static environment variables are [loaded by Vite](https://vitejs.dev/guide/env-and-mode.html#env-files) from `.env` files and `process.env` at build time and then statically injected into your bundle at build time, enabling optimisations like dead code elimination.
 * 
 * **_Private_ access:**
 * 
 * - This module cannot be imported into client-side code
 * - This module only includes variables that _do not_ begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) _and do_ start with [`config.kit.env.privatePrefix`](https://svelte.dev/docs/kit/configuration#env) (if configured)
 * 
 * For example, given the following build time environment:
 * 
 * ```env
 * ENVIRONMENT=production
 * PUBLIC_BASE_URL=http://site.com
 * ```
 * 
 * With the default `publicPrefix` and `privatePrefix`:
 * 
 * ```ts
 * import { ENVIRONMENT, PUBLIC_BASE_URL } from '$env/static/private';
 * 
 * console.log(ENVIRONMENT); // => "production"
 * console.log(PUBLIC_BASE_URL); // => throws error during build
 * ```
 * 
 * The above values will be the same _even if_ different values for `ENVIRONMENT` or `PUBLIC_BASE_URL` are set at runtime, as they are statically replaced in your code with their build time values.
 */
declare module '$env/static/private' {
	export const KDE_FULL_SESSION: string;
	export const TAURI_ENV_PLATFORM: string;
	export const LANGUAGE: string;
	export const PAM_KWALLET5_LOGIN: string;
	export const USER: string;
	export const npm_config_user_agent: string;
	export const TAURI_CLI_VERBOSITY: string;
	export const TEXTDOMAIN: string;
	export const XDG_SEAT: string;
	export const GIT_ASKPASS: string;
	export const XDG_SESSION_TYPE: string;
	export const npm_node_execpath: string;
	export const QT_WAYLAND_RECONNECT: string;
	export const SHLVL: string;
	export const npm_config_noproxy: string;
	export const CHROME_DESKTOP: string;
	export const HOME: string;
	export const KDE_APPLICATIONS_AS_SCOPE: string;
	export const OLDPWD: string;
	export const DESKTOP_SESSION: string;
	export const TERM_PROGRAM_VERSION: string;
	export const npm_package_json: string;
	export const GTK_RC_FILES: string;
	export const KDE_SESSION_VERSION: string;
	export const SDL_IM_MODULE: string;
	export const VSCODE_GIT_ASKPASS_MAIN: string;
	export const XDG_SEAT_PATH: string;
	export const MANAGERPID: string;
	export const VSCODE_GIT_ASKPASS_NODE: string;
	export const npm_config_local_prefix: string;
	export const npm_config_userconfig: string;
	export const DBUS_SESSION_BUS_ADDRESS: string;
	export const IM_CONFIG_CHECK_ENV: string;
	export const SYSTEMD_EXEC_PID: string;
	export const TAURI_ENV_TARGET_TRIPLE: string;
	export const VSCODE_PYTHON_AUTOACTIVATE_GUARD: string;
	export const COLOR: string;
	export const COLORTERM: string;
	export const npm_config_metrics_registry: string;
	export const IM_CONFIG_PHASE: string;
	export const WAYLAND_DISPLAY: string;
	export const XKB_DEFAULT_LAYOUT: string;
	export const GTK_IM_MODULE: string;
	export const LOGNAME: string;
	export const JOURNAL_STREAM: string;
	export const QT_AUTO_SCREEN_SCALE_FACTOR: string;
	export const _: string;
	export const npm_config_prefix: string;
	export const XDG_SESSION_CLASS: string;
	export const TERM: string;
	export const XDG_SESSION_ID: string;
	export const npm_config_cache: string;
	export const FC_FONTATIONS: string;
	export const TAURI_ENV_DEBUG: string;
	export const GTK2_RC_FILES: string;
	export const npm_config_node_gyp: string;
	export const PATH: string;
	export const INVOCATION_ID: string;
	export const NODE: string;
	export const SESSION_MANAGER: string;
	export const TAURI_ENV_PLATFORM_VERSION: string;
	export const npm_package_name: string;
	export const GDK_BACKEND: string;
	export const XDG_MENU_PREFIX: string;
	export const XDG_RUNTIME_DIR: string;
	export const XDG_SESSION_PATH: string;
	export const XKB_DEFAULT_MODEL: string;
	export const DISPLAY: string;
	export const ICEAUTHORITY: string;
	export const LANG: string;
	export const TAURI_ENV_ARCH: string;
	export const XDG_CURRENT_DESKTOP: string;
	export const LS_COLORS: string;
	export const TERM_PROGRAM: string;
	export const VSCODE_GIT_IPC_HANDLE: string;
	export const XAUTHORITY: string;
	export const XDG_SESSION_DESKTOP: string;
	export const XMODIFIERS: string;
	export const npm_lifecycle_script: string;
	export const SSH_AUTH_SOCK: string;
	export const SHELL: string;
	export const npm_lifecycle_event: string;
	export const npm_package_version: string;
	export const NO_AT_BRIDGE: string;
	export const GPG_AGENT_INFO: string;
	export const QT_IM_MODULE: string;
	export const TAURI_ENV_FAMILY: string;
	export const VSCODE_GIT_ASKPASS_EXTRA_ARGS: string;
	export const XDG_VTNR: string;
	export const npm_config_globalconfig: string;
	export const npm_config_init_module: string;
	export const PWD: string;
	export const npm_config_globalignorefile: string;
	export const npm_execpath: string;
	export const CLUTTER_IM_MODULE: string;
	export const XDG_CONFIG_DIRS: string;
	export const npm_config_global_prefix: string;
	export const npm_command: string;
	export const KDE_SESSION_UID: string;
	export const EDITOR: string;
	export const INIT_CWD: string;
	export const NODE_ENV: string;
}

/**
 * This module provides access to environment variables that are injected _statically_ into your bundle at build time and are _publicly_ accessible.
 * 
 * |         | Runtime                                                                    | Build time                                                               |
 * | ------- | -------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
 * | Private | [`$env/dynamic/private`](https://svelte.dev/docs/kit/$env-dynamic-private) | [`$env/static/private`](https://svelte.dev/docs/kit/$env-static-private) |
 * | Public  | [`$env/dynamic/public`](https://svelte.dev/docs/kit/$env-dynamic-public)   | [`$env/static/public`](https://svelte.dev/docs/kit/$env-static-public)   |
 * 
 * Static environment variables are [loaded by Vite](https://vitejs.dev/guide/env-and-mode.html#env-files) from `.env` files and `process.env` at build time and then statically injected into your bundle at build time, enabling optimisations like dead code elimination.
 * 
 * **_Public_ access:**
 * 
 * - This module _can_ be imported into client-side code
 * - **Only** variables that begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) (which defaults to `PUBLIC_`) are included
 * 
 * For example, given the following build time environment:
 * 
 * ```env
 * ENVIRONMENT=production
 * PUBLIC_BASE_URL=http://site.com
 * ```
 * 
 * With the default `publicPrefix` and `privatePrefix`:
 * 
 * ```ts
 * import { ENVIRONMENT, PUBLIC_BASE_URL } from '$env/static/public';
 * 
 * console.log(ENVIRONMENT); // => throws error during build
 * console.log(PUBLIC_BASE_URL); // => "http://site.com"
 * ```
 * 
 * The above values will be the same _even if_ different values for `ENVIRONMENT` or `PUBLIC_BASE_URL` are set at runtime, as they are statically replaced in your code with their build time values.
 */
declare module '$env/static/public' {
	
}

/**
 * This module provides access to environment variables set _dynamically_ at runtime and that are limited to _private_ access.
 * 
 * |         | Runtime                                                                    | Build time                                                               |
 * | ------- | -------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
 * | Private | [`$env/dynamic/private`](https://svelte.dev/docs/kit/$env-dynamic-private) | [`$env/static/private`](https://svelte.dev/docs/kit/$env-static-private) |
 * | Public  | [`$env/dynamic/public`](https://svelte.dev/docs/kit/$env-dynamic-public)   | [`$env/static/public`](https://svelte.dev/docs/kit/$env-static-public)   |
 * 
 * Dynamic environment variables are defined by the platform you're running on. For example if you're using [`adapter-node`](https://github.com/sveltejs/kit/tree/main/packages/adapter-node) (or running [`vite preview`](https://svelte.dev/docs/kit/cli)), this is equivalent to `process.env`.
 * 
 * **_Private_ access:**
 * 
 * - This module cannot be imported into client-side code
 * - This module includes variables that _do not_ begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) _and do_ start with [`config.kit.env.privatePrefix`](https://svelte.dev/docs/kit/configuration#env) (if configured)
 * 
 * > [!NOTE] In `dev`, `$env/dynamic` includes environment variables from `.env`. In `prod`, this behavior will depend on your adapter.
 * 
 * > [!NOTE] To get correct types, environment variables referenced in your code should be declared (for example in an `.env` file), even if they don't have a value until the app is deployed:
 * >
 * > ```env
 * > MY_FEATURE_FLAG=
 * > ```
 * >
 * > You can override `.env` values from the command line like so:
 * >
 * > ```sh
 * > MY_FEATURE_FLAG="enabled" npm run dev
 * > ```
 * 
 * For example, given the following runtime environment:
 * 
 * ```env
 * ENVIRONMENT=production
 * PUBLIC_BASE_URL=http://site.com
 * ```
 * 
 * With the default `publicPrefix` and `privatePrefix`:
 * 
 * ```ts
 * import { env } from '$env/dynamic/private';
 * 
 * console.log(env.ENVIRONMENT); // => "production"
 * console.log(env.PUBLIC_BASE_URL); // => undefined
 * ```
 */
declare module '$env/dynamic/private' {
	export const env: {
		KDE_FULL_SESSION: string;
		TAURI_ENV_PLATFORM: string;
		LANGUAGE: string;
		PAM_KWALLET5_LOGIN: string;
		USER: string;
		npm_config_user_agent: string;
		TAURI_CLI_VERBOSITY: string;
		TEXTDOMAIN: string;
		XDG_SEAT: string;
		GIT_ASKPASS: string;
		XDG_SESSION_TYPE: string;
		npm_node_execpath: string;
		QT_WAYLAND_RECONNECT: string;
		SHLVL: string;
		npm_config_noproxy: string;
		CHROME_DESKTOP: string;
		HOME: string;
		KDE_APPLICATIONS_AS_SCOPE: string;
		OLDPWD: string;
		DESKTOP_SESSION: string;
		TERM_PROGRAM_VERSION: string;
		npm_package_json: string;
		GTK_RC_FILES: string;
		KDE_SESSION_VERSION: string;
		SDL_IM_MODULE: string;
		VSCODE_GIT_ASKPASS_MAIN: string;
		XDG_SEAT_PATH: string;
		MANAGERPID: string;
		VSCODE_GIT_ASKPASS_NODE: string;
		npm_config_local_prefix: string;
		npm_config_userconfig: string;
		DBUS_SESSION_BUS_ADDRESS: string;
		IM_CONFIG_CHECK_ENV: string;
		SYSTEMD_EXEC_PID: string;
		TAURI_ENV_TARGET_TRIPLE: string;
		VSCODE_PYTHON_AUTOACTIVATE_GUARD: string;
		COLOR: string;
		COLORTERM: string;
		npm_config_metrics_registry: string;
		IM_CONFIG_PHASE: string;
		WAYLAND_DISPLAY: string;
		XKB_DEFAULT_LAYOUT: string;
		GTK_IM_MODULE: string;
		LOGNAME: string;
		JOURNAL_STREAM: string;
		QT_AUTO_SCREEN_SCALE_FACTOR: string;
		_: string;
		npm_config_prefix: string;
		XDG_SESSION_CLASS: string;
		TERM: string;
		XDG_SESSION_ID: string;
		npm_config_cache: string;
		FC_FONTATIONS: string;
		TAURI_ENV_DEBUG: string;
		GTK2_RC_FILES: string;
		npm_config_node_gyp: string;
		PATH: string;
		INVOCATION_ID: string;
		NODE: string;
		SESSION_MANAGER: string;
		TAURI_ENV_PLATFORM_VERSION: string;
		npm_package_name: string;
		GDK_BACKEND: string;
		XDG_MENU_PREFIX: string;
		XDG_RUNTIME_DIR: string;
		XDG_SESSION_PATH: string;
		XKB_DEFAULT_MODEL: string;
		DISPLAY: string;
		ICEAUTHORITY: string;
		LANG: string;
		TAURI_ENV_ARCH: string;
		XDG_CURRENT_DESKTOP: string;
		LS_COLORS: string;
		TERM_PROGRAM: string;
		VSCODE_GIT_IPC_HANDLE: string;
		XAUTHORITY: string;
		XDG_SESSION_DESKTOP: string;
		XMODIFIERS: string;
		npm_lifecycle_script: string;
		SSH_AUTH_SOCK: string;
		SHELL: string;
		npm_lifecycle_event: string;
		npm_package_version: string;
		NO_AT_BRIDGE: string;
		GPG_AGENT_INFO: string;
		QT_IM_MODULE: string;
		TAURI_ENV_FAMILY: string;
		VSCODE_GIT_ASKPASS_EXTRA_ARGS: string;
		XDG_VTNR: string;
		npm_config_globalconfig: string;
		npm_config_init_module: string;
		PWD: string;
		npm_config_globalignorefile: string;
		npm_execpath: string;
		CLUTTER_IM_MODULE: string;
		XDG_CONFIG_DIRS: string;
		npm_config_global_prefix: string;
		npm_command: string;
		KDE_SESSION_UID: string;
		EDITOR: string;
		INIT_CWD: string;
		NODE_ENV: string;
		[key: `PUBLIC_${string}`]: undefined;
		[key: `${string}`]: string | undefined;
	}
}

/**
 * This module provides access to environment variables set _dynamically_ at runtime and that are _publicly_ accessible.
 * 
 * |         | Runtime                                                                    | Build time                                                               |
 * | ------- | -------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
 * | Private | [`$env/dynamic/private`](https://svelte.dev/docs/kit/$env-dynamic-private) | [`$env/static/private`](https://svelte.dev/docs/kit/$env-static-private) |
 * | Public  | [`$env/dynamic/public`](https://svelte.dev/docs/kit/$env-dynamic-public)   | [`$env/static/public`](https://svelte.dev/docs/kit/$env-static-public)   |
 * 
 * Dynamic environment variables are defined by the platform you're running on. For example if you're using [`adapter-node`](https://github.com/sveltejs/kit/tree/main/packages/adapter-node) (or running [`vite preview`](https://svelte.dev/docs/kit/cli)), this is equivalent to `process.env`.
 * 
 * **_Public_ access:**
 * 
 * - This module _can_ be imported into client-side code
 * - **Only** variables that begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) (which defaults to `PUBLIC_`) are included
 * 
 * > [!NOTE] In `dev`, `$env/dynamic` includes environment variables from `.env`. In `prod`, this behavior will depend on your adapter.
 * 
 * > [!NOTE] To get correct types, environment variables referenced in your code should be declared (for example in an `.env` file), even if they don't have a value until the app is deployed:
 * >
 * > ```env
 * > MY_FEATURE_FLAG=
 * > ```
 * >
 * > You can override `.env` values from the command line like so:
 * >
 * > ```sh
 * > MY_FEATURE_FLAG="enabled" npm run dev
 * > ```
 * 
 * For example, given the following runtime environment:
 * 
 * ```env
 * ENVIRONMENT=production
 * PUBLIC_BASE_URL=http://example.com
 * ```
 * 
 * With the default `publicPrefix` and `privatePrefix`:
 * 
 * ```ts
 * import { env } from '$env/dynamic/public';
 * console.log(env.ENVIRONMENT); // => undefined, not public
 * console.log(env.PUBLIC_BASE_URL); // => "http://example.com"
 * ```
 * 
 * ```
 * 
 * ```
 */
declare module '$env/dynamic/public' {
	export const env: {
		[key: `PUBLIC_${string}`]: string | undefined;
	}
}

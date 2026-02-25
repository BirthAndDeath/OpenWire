
// this file is generated — do not edit it


/// <reference types="@sveltejs/kit" />

/**
 * Environment variables [loaded by Vite](https://vitejs.dev/guide/env-and-mode.html#env-files) from `.env` files and `process.env`. Like [`$env/dynamic/private`](https://svelte.dev/docs/kit/$env-dynamic-private), this module cannot be imported into client-side code. This module only includes variables that _do not_ begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) _and do_ start with [`config.kit.env.privatePrefix`](https://svelte.dev/docs/kit/configuration#env) (if configured).
 * 
 * _Unlike_ [`$env/dynamic/private`](https://svelte.dev/docs/kit/$env-dynamic-private), the values exported from this module are statically injected into your bundle at build time, enabling optimisations like dead code elimination.
 * 
 * ```ts
 * import { API_KEY } from '$env/static/private';
 * ```
 * 
 * Note that all environment variables referenced in your code should be declared (for example in an `.env` file), even if they don't have a value until the app is deployed:
 * 
 * ```
 * MY_FEATURE_FLAG=""
 * ```
 * 
 * You can override `.env` values from the command line like so:
 * 
 * ```sh
 * MY_FEATURE_FLAG="enabled" npm run dev
 * ```
 */
declare module '$env/static/private' {
	export const TAURI_ENV_PLATFORM: string;
	export const LANGUAGE: string;
	export const PAM_KWALLET5_LOGIN: string;
	export const USER: string;
	export const npm_config_user_agent: string;
	export const TAURI_CLI_VERBOSITY: string;
	export const TEXTDOMAIN: string;
	export const XDG_SEAT: string;
	export const GIT_ASKPASS: string;
	export const SSH_AGENT_PID: string;
	export const XDG_SESSION_TYPE: string;
	export const npm_node_execpath: string;
	export const SHLVL: string;
	export const XDG_CACHE_HOME: string;
	export const npm_config_noproxy: string;
	export const CHROME_DESKTOP: string;
	export const HOME: string;
	export const OLDPWD: string;
	export const DESKTOP_SESSION: string;
	export const TERM_PROGRAM_VERSION: string;
	export const npm_package_json: string;
	export const PANEL_GDK_CORE_DEVICE_EVENTS: string;
	export const GTK_MODULES: string;
	export const SDL_IM_MODULE: string;
	export const VSCODE_GIT_ASKPASS_MAIN: string;
	export const XDG_SEAT_PATH: string;
	export const VSCODE_GIT_ASKPASS_NODE: string;
	export const npm_config_local_prefix: string;
	export const npm_config_userconfig: string;
	export const DBUS_SESSION_BUS_ADDRESS: string;
	export const TAURI_ENV_TARGET_TRIPLE: string;
	export const VSCODE_PYTHON_AUTOACTIVATE_GUARD: string;
	export const COLOR: string;
	export const COLORTERM: string;
	export const UBUNTU_MENUPROXY: string;
	export const npm_config_metrics_registry: string;
	export const QT_QPA_PLATFORMTHEME: string;
	export const GTK_IM_MODULE: string;
	export const LOGNAME: string;
	export const _: string;
	export const npm_config_prefix: string;
	export const XDG_SESSION_CLASS: string;
	export const TERM: string;
	export const XDG_SESSION_ID: string;
	export const npm_config_cache: string;
	export const FC_FONTATIONS: string;
	export const TAURI_ENV_DEBUG: string;
	export const CODELLDB_LAUNCH_CONNECT_FILE: string;
	export const npm_config_node_gyp: string;
	export const PATH: string;
	export const NODE: string;
	export const SESSION_MANAGER: string;
	export const TAURI_ENV_PLATFORM_VERSION: string;
	export const npm_package_name: string;
	export const GDK_BACKEND: string;
	export const XDG_MENU_PREFIX: string;
	export const XDG_RUNTIME_DIR: string;
	export const XDG_SESSION_PATH: string;
	export const DISPLAY: string;
	export const LANG: string;
	export const TAURI_ENV_ARCH: string;
	export const XDG_CURRENT_DESKTOP: string;
	export const LS_COLORS: string;
	export const TERM_PROGRAM: string;
	export const VSCODE_GIT_IPC_HANDLE: string;
	export const XAUTHORITY: string;
	export const XDG_CONFIG_HOME: string;
	export const XDG_SESSION_DESKTOP: string;
	export const XMODIFIERS: string;
	export const npm_lifecycle_script: string;
	export const SSH_AUTH_SOCK: string;
	export const SHELL: string;
	export const npm_lifecycle_event: string;
	export const npm_package_version: string;
	export const NO_AT_BRIDGE: string;
	export const QT_IM_MODULE: string;
	export const TAURI_ENV_FAMILY: string;
	export const VSCODE_GIT_ASKPASS_EXTRA_ARGS: string;
	export const XDG_VTNR: string;
	export const npm_config_globalconfig: string;
	export const npm_config_init_module: string;
	export const LC_ALL: string;
	export const PWD: string;
	export const npm_config_globalignorefile: string;
	export const npm_execpath: string;
	export const CLUTTER_IM_MODULE: string;
	export const XDG_CONFIG_DIRS: string;
	export const XDG_DATA_DIRS: string;
	export const npm_config_global_prefix: string;
	export const npm_command: string;
	export const EDITOR: string;
	export const INIT_CWD: string;
	export const NODE_ENV: string;
}

/**
 * Similar to [`$env/static/private`](https://svelte.dev/docs/kit/$env-static-private), except that it only includes environment variables that begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) (which defaults to `PUBLIC_`), and can therefore safely be exposed to client-side code.
 * 
 * Values are replaced statically at build time.
 * 
 * ```ts
 * import { PUBLIC_BASE_URL } from '$env/static/public';
 * ```
 */
declare module '$env/static/public' {
	
}

/**
 * This module provides access to runtime environment variables, as defined by the platform you're running on. For example if you're using [`adapter-node`](https://github.com/sveltejs/kit/tree/main/packages/adapter-node) (or running [`vite preview`](https://svelte.dev/docs/kit/cli)), this is equivalent to `process.env`. This module only includes variables that _do not_ begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) _and do_ start with [`config.kit.env.privatePrefix`](https://svelte.dev/docs/kit/configuration#env) (if configured).
 * 
 * This module cannot be imported into client-side code.
 * 
 * ```ts
 * import { env } from '$env/dynamic/private';
 * console.log(env.DEPLOYMENT_SPECIFIC_VARIABLE);
 * ```
 * 
 * > [!NOTE] In `dev`, `$env/dynamic` always includes environment variables from `.env`. In `prod`, this behavior will depend on your adapter.
 */
declare module '$env/dynamic/private' {
	export const env: {
		TAURI_ENV_PLATFORM: string;
		LANGUAGE: string;
		PAM_KWALLET5_LOGIN: string;
		USER: string;
		npm_config_user_agent: string;
		TAURI_CLI_VERBOSITY: string;
		TEXTDOMAIN: string;
		XDG_SEAT: string;
		GIT_ASKPASS: string;
		SSH_AGENT_PID: string;
		XDG_SESSION_TYPE: string;
		npm_node_execpath: string;
		SHLVL: string;
		XDG_CACHE_HOME: string;
		npm_config_noproxy: string;
		CHROME_DESKTOP: string;
		HOME: string;
		OLDPWD: string;
		DESKTOP_SESSION: string;
		TERM_PROGRAM_VERSION: string;
		npm_package_json: string;
		PANEL_GDK_CORE_DEVICE_EVENTS: string;
		GTK_MODULES: string;
		SDL_IM_MODULE: string;
		VSCODE_GIT_ASKPASS_MAIN: string;
		XDG_SEAT_PATH: string;
		VSCODE_GIT_ASKPASS_NODE: string;
		npm_config_local_prefix: string;
		npm_config_userconfig: string;
		DBUS_SESSION_BUS_ADDRESS: string;
		TAURI_ENV_TARGET_TRIPLE: string;
		VSCODE_PYTHON_AUTOACTIVATE_GUARD: string;
		COLOR: string;
		COLORTERM: string;
		UBUNTU_MENUPROXY: string;
		npm_config_metrics_registry: string;
		QT_QPA_PLATFORMTHEME: string;
		GTK_IM_MODULE: string;
		LOGNAME: string;
		_: string;
		npm_config_prefix: string;
		XDG_SESSION_CLASS: string;
		TERM: string;
		XDG_SESSION_ID: string;
		npm_config_cache: string;
		FC_FONTATIONS: string;
		TAURI_ENV_DEBUG: string;
		CODELLDB_LAUNCH_CONNECT_FILE: string;
		npm_config_node_gyp: string;
		PATH: string;
		NODE: string;
		SESSION_MANAGER: string;
		TAURI_ENV_PLATFORM_VERSION: string;
		npm_package_name: string;
		GDK_BACKEND: string;
		XDG_MENU_PREFIX: string;
		XDG_RUNTIME_DIR: string;
		XDG_SESSION_PATH: string;
		DISPLAY: string;
		LANG: string;
		TAURI_ENV_ARCH: string;
		XDG_CURRENT_DESKTOP: string;
		LS_COLORS: string;
		TERM_PROGRAM: string;
		VSCODE_GIT_IPC_HANDLE: string;
		XAUTHORITY: string;
		XDG_CONFIG_HOME: string;
		XDG_SESSION_DESKTOP: string;
		XMODIFIERS: string;
		npm_lifecycle_script: string;
		SSH_AUTH_SOCK: string;
		SHELL: string;
		npm_lifecycle_event: string;
		npm_package_version: string;
		NO_AT_BRIDGE: string;
		QT_IM_MODULE: string;
		TAURI_ENV_FAMILY: string;
		VSCODE_GIT_ASKPASS_EXTRA_ARGS: string;
		XDG_VTNR: string;
		npm_config_globalconfig: string;
		npm_config_init_module: string;
		LC_ALL: string;
		PWD: string;
		npm_config_globalignorefile: string;
		npm_execpath: string;
		CLUTTER_IM_MODULE: string;
		XDG_CONFIG_DIRS: string;
		XDG_DATA_DIRS: string;
		npm_config_global_prefix: string;
		npm_command: string;
		EDITOR: string;
		INIT_CWD: string;
		NODE_ENV: string;
		[key: `PUBLIC_${string}`]: undefined;
		[key: `${string}`]: string | undefined;
	}
}

/**
 * Similar to [`$env/dynamic/private`](https://svelte.dev/docs/kit/$env-dynamic-private), but only includes variables that begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) (which defaults to `PUBLIC_`), and can therefore safely be exposed to client-side code.
 * 
 * Note that public dynamic environment variables must all be sent from the server to the client, causing larger network requests — when possible, use `$env/static/public` instead.
 * 
 * ```ts
 * import { env } from '$env/dynamic/public';
 * console.log(env.PUBLIC_DEPLOYMENT_SPECIFIC_VARIABLE);
 * ```
 */
declare module '$env/dynamic/public' {
	export const env: {
		[key: `PUBLIC_${string}`]: string | undefined;
	}
}

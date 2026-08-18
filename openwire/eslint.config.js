import js from "@eslint/js";
import tsParser from "@typescript-eslint/parser";
import tsPlugin from "@typescript-eslint/eslint-plugin";
import globals from "globals";
export default [
    js.configs.recommended,

    {
        ignores: ["**/node_modules/**", "**/build/**", "**/src-tauri/**", "**/.svelte-kit/**"],
    },

    {
        files: ["**/*.js", "**/*.jsx", "**/*.mjs", "**/*.cjs"],
        languageOptions: {
            globals: {
                ...globals.browser,
                ...globals.node,
            },
        },
        rules: {
            // JS 规则
        },
    },

    // TypeScript 文件配置
    {
        files: ["**/*.ts", "**/*.tsx"],
        languageOptions: {
            parser: tsParser,
            globals: {
                ...globals.browser,
                ...globals.node,
            },
        },
        plugins: {
            "@typescript-eslint": tsPlugin,
        },
        rules: {
            ...tsPlugin.configs.recommended.rules,
        },
    },
];
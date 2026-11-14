import en from './locales/en.json'
import zh from './locales/zh.json'
import fr from './locales/fr.json'
import es from './locales/es.json'
import de from './locales/de.json'
import ja from './locales/ja.json'
import { addMessages, getLocaleFromNavigator, init, t } from 'svelte-i18n'
import { get } from 'svelte/store'

addMessages('en', en)
addMessages('zh', zh)
addMessages('fr', fr)
addMessages('es', es)
addMessages('de', de)
addMessages('ja', ja)


init({
    fallbackLocale: 'en',
    initialLocale: getLocaleFromNavigator(),
})

// 非响应式翻译：在异步回调 / 事件函数等非模板上下文中取当前语言文本
// （模板内请直接用 `$_`；opts 的 unknown 值经 never 绕过 svelte-i18n 的类型约束）
export function tt(key: string, opts?: Record<string, unknown>): string {
    return get(t)(key, opts as never)
}
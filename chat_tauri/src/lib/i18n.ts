import en from './locales/en.json'
import zh from './locales/zh.json'
import { addMessages, getLocaleFromNavigator, init } from 'svelte-i18n'

addMessages('en', en)
addMessages('zh', zh)


init({
    fallbackLocale: 'en',
    initialLocale: getLocaleFromNavigator(),
})
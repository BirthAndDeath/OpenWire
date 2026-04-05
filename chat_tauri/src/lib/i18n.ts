import en from './locales/en.json'
import zh from './locales/zh.json'
import fr from './locales/fr.json'
import es from './locales/es.json'
import de from './locales/de.json'
import ja from './locales/ja.json'
import { addMessages, getLocaleFromNavigator, init } from 'svelte-i18n'

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
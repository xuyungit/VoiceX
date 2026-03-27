import { createApp } from 'vue'
import { createPinia } from 'pinia'
import {
    create,
    NConfigProvider,
    NMessageProvider,
    NDialogProvider
} from 'naive-ui'
import { i18n } from './i18n'
import router from './router'
import App from './App.vue'
import './styles/main.css'

// Create Naive UI with necessary components
const naive = create({
    components: [
        NConfigProvider,
        NMessageProvider,
        NDialogProvider
    ]
})

const app = createApp(App)

app.use(createPinia())
app.use(router)
app.use(naive)
app.use(i18n)

app.mount('#app')

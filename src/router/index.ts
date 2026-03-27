import { createRouter, createWebHistory } from 'vue-router'

const router = createRouter({
  history: createWebHistory(),
  routes: [
    {
      path: '/',
      redirect: '/overview'
    },
    {
      path: '/overview',
      name: 'overview',
      component: () => import('../views/Overview.vue')
    },
    {
      path: '/history',
      name: 'history',
      component: () => import('../views/History.vue')
    },
    {
      path: '/dictionary',
      name: 'dictionary',
      component: () => import('../views/Dictionary.vue')
    },
    {
      path: '/hotwords',
      name: 'hotwords',
      component: () => import('../views/Hotwords.vue')
    },
    {
      path: '/asr-settings',
      name: 'asr-settings',
      component: () => import('../views/AsrSettings.vue')
    },
    {
      path: '/llm-settings',
      name: 'llm-settings',
      component: () => import('../views/LlmSettings.vue')
    },
    {
      path: '/input-settings',
      name: 'input-settings',
      component: () => import('../views/InputSettings.vue')
    },
    {
      path: '/sync',
      name: 'sync',
      component: () => import('../views/SyncSettings.vue')
    },
    {
      path: '/post-processing',
      name: 'post-processing',
      component: () => import('../views/PostProcessing.vue')
    },
    {
      path: '/about',
      name: 'about',
      component: () => import('../views/About.vue')
    }
  ]
})

export default router

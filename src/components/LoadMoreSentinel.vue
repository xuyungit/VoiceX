<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { NSpin } from 'naive-ui'

// Placed after a paged list: asks for the next page as it nears the visible
// area of whichever ancestor scrolls. Render it only while more pages exist
// and the last load did not fail, so a failure cannot retry in a loop.
const props = defineProps<{ loading: boolean }>()
const emit = defineEmits<{ load: [] }>()

const el = ref<HTMLElement | null>(null)
let observer: IntersectionObserver | null = null

function scrollParent(node: HTMLElement): HTMLElement | null {
  for (let parent = node.parentElement; parent; parent = parent.parentElement) {
    const { overflowY } = getComputedStyle(parent)
    if (overflowY === 'auto' || overflowY === 'scroll') return parent
  }
  return null
}

onMounted(() => {
  if (!el.value) return
  observer = new IntersectionObserver(entries => {
    if (entries.some(entry => entry.isIntersecting) && !props.loading) emit('load')
  }, { root: scrollParent(el.value), rootMargin: '0px 0px 400px 0px' })
  observer.observe(el.value)
})

// A page that does not fill the view leaves the marker visible, which the
// observer does not report again; re-observing reports its current state.
watch(() => props.loading, loading => {
  if (loading || !observer || !el.value) return
  observer.unobserve(el.value)
  observer.observe(el.value)
})

onBeforeUnmount(() => observer?.disconnect())
</script>

<template>
  <div ref="el" class="load-more-sentinel" aria-hidden="true">
    <NSpin v-if="loading" size="small" />
  </div>
</template>

<style scoped>
.load-more-sentinel { display: flex; justify-content: center; min-height: 1px; padding: 12px 0; }
</style>

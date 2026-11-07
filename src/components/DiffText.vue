<script lang="ts">
import { computed, defineComponent, h, type PropType, type VNodeChild } from 'vue'
import { condense, expand, type TextDiff } from '../utils/textDiff'

// Renders a TextDiff. `inline` shows removals and insertions in one paragraph;
// `before` / `after` show one side with its changes marked (stacked layout).
// `context`, when set, shortens unchanged text to that many characters around
// each change. Every change is wrapped in a `.diff-change` element carrying
// `data-change`, its index in `diff.segments`, so per-change actions (such as
// saving a correction as a hotword or replacement rule) can resolve it.
//
// A render function rather than a template: the text is shown with
// `white-space: pre-wrap`, and template whitespace handling would alter it.
export default defineComponent({
  name: 'DiffText',
  props: {
    diff: { type: Object as PropType<TextDiff>, required: true },
    view: { type: String as PropType<'inline' | 'before' | 'after'>, required: true },
    context: { type: Number, default: undefined }
  },
  setup(props) {
    const parts = computed(() => props.context === undefined ? expand(props.diff) : condense(props.diff, props.context))
    // Line breaks inside a change get a visible mark; whitespace-only changes
    // get a box so an added or removed space is not invisible.
    const mark = (tag: 'del' | 'ins', text: string) =>
      h(tag, { class: { blank: !/\S/.test(text) } }, text.replace(/\n/g, '↵\n'))

    return () => {
      const children: VNodeChild[] = []
      for (const part of parts.value) {
        if (part.kind === 'equal') children.push(part.text)
        else if (part.kind === 'gap') children.push(h('span', { class: 'diff-gap', 'aria-hidden': 'true' }, '…'))
        else if (!props.diff.aligned && props.view !== 'inline') {
          // Nothing lines up, so marking the whole side adds only noise.
          children.push(props.view === 'before' ? part.before : part.after)
        } else {
          const marks = []
          if (part.before && props.view !== 'after') marks.push(mark('del', part.before))
          if (part.after && props.view !== 'before') marks.push(mark('ins', part.after))
          if (marks.length) children.push(h('span', { class: 'diff-change', 'data-change': part.index }, marks))
        }
      }
      return h('span', { class: 'diff-text' }, children)
    }
  }
})
</script>

<style scoped>
.diff-text { white-space: pre-wrap; overflow-wrap: anywhere; }
del {
  color: var(--color-error);
  text-decoration: line-through;
  text-decoration-color: color-mix(in srgb, var(--color-error) 70%, transparent);
}
ins {
  color: inherit;
  text-decoration: none;
  border-radius: 3px;
  background: color-mix(in srgb, var(--color-success) 20%, transparent);
}
del.blank { border-radius: 3px; background: color-mix(in srgb, var(--color-error) 16%, transparent); }
.blank { padding: 0 0.15em; }
.diff-gap { padding: 0 0.3em; color: var(--color-text-tertiary); }
</style>

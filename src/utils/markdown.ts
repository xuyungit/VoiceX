import MarkdownIt from 'markdown-it'

// Release notes arrive from a remote update manifest, so raw HTML stays
// escaped: anything rendered into the webview could reach Tauri commands.
// markdown-it also refuses javascript:/vbscript:/file: link targets.
const md = new MarkdownIt({ html: false, linkify: true })

export function renderMarkdown(source: string): string {
  return md.render(source)
}

// Run with playwright-cli run-code --filename=scripts/screenshots/capture.js
async (page) => {
  const context = await page.context().browser().newContext({
    viewport: { width: 1120, height: 760 }, deviceScaleFactor: 2,
    locale: 'en-US', timezoneId: 'Asia/Singapore', reducedMotion: 'reduce',
  })
  // Only the local fixture may receive requests, including fonts and images.
  await context.route('**/*', route => {
    const url = new URL(route.request().url())
    return url.origin === 'http://127.0.0.1:1523' ? route.continue() : route.abort()
  })
  const capturePage = await context.newPage()
  await capturePage.clock.setFixedTime(new Date('2026-11-05T10:30:00+08:00'))
  const errors = []
  capturePage.on('pageerror', error => errors.push(String(error)))
  capturePage.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
  try {
    for (const lang of ['zh', 'en']) {
      for (const [route, filename, theme] of [
        ['overview', 'main-window', 'light'],
        ['history', 'history-dark', 'dark'],
        ['asr-settings', 'asr-settings', 'light'],
        ['input-settings', 'input-settings', 'light'],
        ['reading-settings', 'reading-settings', 'light'],
      ]) {
        await capturePage.setViewportSize({ width: 1120, height: route === 'reading-settings' ? 650 : route === 'input-settings' ? 700 : 760 })
        await capturePage.goto(`http://127.0.0.1:1523/${route}?lang=${lang}&theme=${theme}`)
        await capturePage.locator(`html[lang="${lang === 'zh' ? 'zh-CN' : 'en-US'}"][data-theme="${theme}"]`).waitFor()
        await capturePage.locator('.page-title').waitFor()
        await capturePage.evaluate(() => document.fonts.ready)
        // Wait for Vue's route transition and initial backend fixture reads.
        await capturePage.waitForTimeout(400)
        if (route === 'history') {
          await capturePage.locator('.list-text').first().hover({ position: { x: 20, y: 8 } })
          await capturePage.waitForTimeout(200)
        } else {
          await capturePage.mouse.move(0, 0)
        }
        if (errors.length) throw new Error(errors.join('\n'))
        await capturePage.screenshot({ path: `assets/screenshots/${lang}/${filename}.png`, animations: 'disabled' })
      }
      // Match STREAM_HUD_WIDTH / STREAM_HUD_HEIGHT in src-tauri/src/hud/window.rs.
      await capturePage.setViewportSize({ width: 256, height: 100 })
      await capturePage.goto(`http://127.0.0.1:1523/src/hud/index.html?lang=${lang}`)
      await capturePage.locator('body:not(.hud-loading)').waitFor()
      await capturePage.locator('#textArea').filter({ hasText: 'VoiceX' }).waitFor()
      if (errors.length) throw new Error(errors.join('\n'))
      await capturePage.screenshot({ path: `assets/screenshots/${lang}/hud-window.png`, animations: 'disabled' })
    }
    console.log('Captured 12 bilingual screenshots; no browser errors.')
  } finally {
    await context.close()
  }
}

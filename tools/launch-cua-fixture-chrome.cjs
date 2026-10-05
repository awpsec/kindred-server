// Fixture lifecycle only; production actions remain in the Rust guest executor.
const fs = require('fs');
const { chromium } = require(process.env.KINDRED_PLAYWRIGHT_MODULE);
(async () => {
  console.log('Fixture browser launch', Date.now());
  const context = await chromium.launchPersistentContext(process.argv[2], {
    executablePath: process.env.KINDRED_TEST_CHROME,
    headless: false, viewport: null, handleSIGTERM: false,
    ignoreHTTPSErrors: true,
    args: ['--no-sandbox', '--ozone-platform=x11', '--disable-dev-shm-usage',
      '--force-renderer-accessibility', '--remote-debugging-port=0',
      '--remote-debugging-address=127.0.0.1', '--ignore-certificate-errors'],
  });
  console.log('Fixture browser launched; navigating', Date.now(), process.argv[3]);
  context.pages()[0].on('pageerror', error => console.error('Fixture page error', error));
  await context.pages()[0].goto(process.argv[3], { timeout: 20000 });
  console.log('Fixture browser ready', Date.now());
  fs.writeFileSync(process.argv[4], 'ready');
  process.stdin.resume();
  process.on('SIGTERM', async () => { await context.close(); process.exit(0); });
})().catch(error => { console.error(error); process.exit(1); });

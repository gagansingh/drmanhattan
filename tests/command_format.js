// Cases for the dashboard's command formatter (assets/command-format.js). tests/dashboard.rs
// runs them with node after the page's `esc` and the formatter; prints `ok N` or throws.

const text = (html) => html.replace(/<br>/g, '\n').replace(/<[^>]*>/g, '').replace(/&#39;/g, "'")
  .replace(/&quot;/g, '"').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&');
const has = (html, cls, word) => {
  const re = new RegExp(`<span class="${cls}"[^>]*>(?:<[^>]*>)*${esc(word).replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`);
  if (!re.test(html)) throw new Error(`expected ${cls} on ${JSON.stringify(word)} in ${html}`);
};
const eq = (got, want, what) => {
  if (got !== want) throw new Error(`${what}: got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
};
const ctx = { home: '/Users/me' };
let n = 0;
const check = (fn) => { fn(); n += 1; };

check(() => {
  const h = commandHtml(['swift', 'test', '--filter', 'Foo'], '/Users/me/app', ctx);
  eq(text(h), 'swift test --filter Foo', 'plain');
  has(h, 'c-prog', 'swift');
  has(h, 'c-sub', 'test');
  has(h, 'c-flag', '--filter');
});

check(() => {
  const h = commandHtml(['env', 'DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer', 'swift', 'build', '-Xswiftc', '-cache-compile-job'], '/w', ctx);
  eq(text(h), 'env DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift build -Xswiftc -cache-compile-job', 'env');
  has(h, 'c-dim', 'env');
  has(h, 'c-key', 'DEVELOPER_DIR');
  has(h, 'c-prog', 'swift');
  has(h, 'c-sub', 'build');
});

check(() => {
  const h = commandHtml(['sh', '-c', 'cd app && npm test 2>&1 | tee out.log'], '/w', ctx);
  eq(text(h), 'sh -c cd app && npm test 2>&1 | tee out.log', 'sh -c');
  has(h, 'c-dim', 'sh -c');
  has(h, 'c-prog', 'cd');
  has(h, 'c-prog', 'npm');
  has(h, 'c-sub', 'test');
  has(h, 'c-op', '&&');
  has(h, 'c-op', '2>&1');
  has(h, 'c-op', '|');
  has(h, 'c-prog', 'tee');
});

check(() => {
  const argv = ['bash', '/x/bin/wrapper.sh', '--phase', 'run', '--cwd', '/y', '--', 'xcodebuild', '-scheme', 'App',
    '-destination', 'generic/platform=iOS Simulator', 'build'];
  const h = commandHtml(argv, '/y', ctx);
  eq(text(h), "via wrapper.sh xcodebuild -scheme App -destination 'generic/platform=iOS Simulator' build", 'via');
  has(h, 'c-via', 'via wrapper.sh');
  if (!h.includes('title="bash /x/bin/wrapper.sh --phase run --cwd /y --"')) throw new Error(`via title: ${h}`);
  has(h, 'c-prog', 'xcodebuild');
  has(h, 'c-flag', '-scheme');
  has(h, 'c-str', "'generic/platform=iOS Simulator'");
});

check(() => {
  // Paths: the job's folder is `.`, the home folder `~`, the full path in the title.
  const h = commandHtml(['/Users/me/app/bin/run', '/Users/me/app/data/in.txt', '/Users/me/other'], '/Users/me/app', ctx);
  eq(text(h), './bin/run ./data/in.txt ~/other', 'paths');
  has(h, 'c-last', 'run');
  if (!h.includes('title="/Users/me/other"')) throw new Error(`path title: ${h}`);
});

check(() => {
  // --flag=value splits at the =; quotes and markup are escaped, never interpreted.
  const h = commandHtml(['make', '--jobs=4', 'MSG=<b>hi</b>', "it's"], '/w', ctx);
  eq(text(h), "make --jobs=4 MSG='<b>hi</b>' 'it'\\''s'", 'escaping');
  has(h, 'c-flag', '--jobs');
  has(h, 'c-key', 'MSG');
  if (h.includes('<b>')) throw new Error('markup was not escaped');
});

check(() => {
  // A long script breaks after && and ;. An unclosed quote falls back to plain text.
  const long = 'cd /Users/me/project/somewhere && cargo build --release --locked && cargo test --workspace --all-features; echo done';
  const h = commandHtml(['sh', '-c', long], '/w', ctx);
  eq((h.match(/<br>/g) || []).length, 3, 'line breaks');
  const broken = commandHtml(['sh', '-c', "echo 'unclosed"], '/w', ctx);
  eq(text(broken), "sh -c echo 'unclosed", 'fallback');
});

check(() => {
  // The runner's own quoting, which the log line is matched against.
  eq(cmdJoin(['sh', '-c', "echo 'a b'"]), `sh -c 'echo '\\''a b'\\'''`, 'join');
  eq(cmdJoin(['xcrun', '--sdk', 'iphonesimulator', 'swift', 'build']), 'xcrun --sdk iphonesimulator swift build', 'join plain');
  const h = commandHtml(['xcrun', '--sdk', 'iphonesimulator', 'swift', 'build'], '/w', ctx);
  has(h, 'c-dim', 'xcrun --sdk iphonesimulator');
  has(h, 'c-prog', 'swift');
});

console.log(`ok ${n}`);

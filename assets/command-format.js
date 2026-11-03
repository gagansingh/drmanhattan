// Commands as a person reads them: commandHtml(argv, cwd). Loaded before the page's own
// script, whose `esc` it uses when called.
/* Commands as a person reads them: the program and its subcommand stand out, wrappers
   (env, xcrun, nice, time, a script that runs `-- <command>`) are dimmed or folded into a
   `via` tag, paths are shortened, and flags, settings, quotes and shell operators each get a
   colour. Colour is emphasis only: the text reads the same without it. Works from the argv
   list, never from a joined string; a `sh -c` script is tokenized with a small tokenizer
   that falls back to plain text. Everything is escaped with esc. */
const CMD_INTERPRETERS = /^(sh|bash|zsh|dash|python[0-9.]*|node|ruby|perl)$/;
const CMD_SHELLS = /^(sh|bash|zsh|dash)$/;
const CMD_ASSIGN = /^[A-Za-z_][A-Za-z0-9_]*=/;
const CMD_OPERATOR = /^(&&|\|\||\||;|>>|>|<|2>&1|&>|2>|&)$/;
const cmdBase = (path) => String(path).replace(/\/+$/, '').split('/').pop() || path;

/** A word as the shell would need it written (the runner's own quoting, for matching). */
function cmdQuote(word) {
  word = String(word);
  return word === '' || /[\s"'$`\\!*?[\]{}()<>|;&#~]/.test(word) ? `'${word.replace(/'/g, `'\\''`)}'` : word;
}
const cmdJoin = (argv) => argv.map(cmdQuote).join(' ');

/** A path, shortened: the job's folder is `.`, the home folder `~`; the folder part dim and
 *  the last part bright, with the whole path in the title. */
function cmdPath(word, ctx) {
  let shown = word;
  if (ctx.cwd && (word === ctx.cwd || word.startsWith(ctx.cwd + '/'))) shown = '.' + word.slice(ctx.cwd.length);
  else if (ctx.home && (word === ctx.home || word.startsWith(ctx.home + '/'))) shown = '~' + word.slice(ctx.home.length);
  const cut = shown.replace(/\/+$/, '').lastIndexOf('/');
  const dir = cut >= 0 ? shown.slice(0, cut + 1) : '';
  const last = cut >= 0 ? shown.slice(cut + 1) : shown;
  const bare = shown.replace(/^~/, ''); // a leading ~ is the shortening, not a character to quote
  const quoted = cmdQuote(bare) !== bare;
  const inner = `${dir ? `<span class="c-dir">${esc(dir)}</span>` : ''}<span class="c-last">${esc(last)}</span>`;
  return `<span class="c-path${quoted ? ' c-str' : ''}" title="${esc(word)}">${quoted ? "'" : ''}${inner}${quoted ? "'" : ''}</span>`;
}

const cmdIsPath = (word) => /^(\/|~\/|\.\.?\/)/.test(word) || (word.includes('/') && !word.includes(' ') && !/^[a-z]+:\/\//i.test(word));

/** One argument after the program. */
function cmdArg(word, ctx) {
  const flag = /^(--?[A-Za-z][\w.-]*)(=)(.*)$/s.exec(word);
  if (flag) return `<span class="c-flag">${esc(flag[1])}</span><span class="c-dim">=</span>${cmdValue(flag[3], ctx)}`;
  if (/^--?[A-Za-z]/.test(word) && !/\s/.test(word)) return `<span class="c-flag">${esc(word)}</span>`;
  if (CMD_ASSIGN.test(word)) return cmdAssign(word, ctx);
  return cmdValue(word, ctx);
}

function cmdValue(word, ctx) {
  if (cmdIsPath(word)) return cmdPath(word, ctx);
  const quoted = cmdQuote(word);
  return quoted === word ? esc(word) : `<span class="c-str">${esc(quoted)}</span>`;
}

function cmdAssign(word, ctx) {
  const at = word.indexOf('=');
  return `<span class="c-key">${esc(word.slice(0, at))}</span><span class="c-dim">=</span>${cmdValue(word.slice(at + 1), ctx)}`;
}

/** The program, bold, and its first plain word, the subcommand. */
function cmdProgram(argv, ctx) {
  const [program, ...args] = argv;
  const name = cmdIsPath(program) ? cmdPath(program, ctx) : esc(program);
  const out = [`<span class="c-prog">${name}</span>`];
  args.forEach((word, i) => {
    out.push(i === 0 && /^[a-z][\w:-]*$/i.test(word) ? `<span class="c-sub">${esc(word)}</span>` : cmdArg(word, ctx));
  });
  return out.join(' ');
}

/** Splits a shell script into words (quotes kept), operators and spaces; null when it cannot. */
function cmdTokens(script) {
  const tokens = [];
  let i = 0;
  while (i < script.length) {
    const rest = script.slice(i);
    const space = /^\s+/.exec(rest);
    if (space) { tokens.push({ kind: 'space', text: space[0] }); i += space[0].length; continue; }
    const op = /^(2>&1|&&|\|\||>>|&>|2>|[|;&<>])/.exec(rest);
    if (op) { tokens.push({ kind: 'op', text: op[0] }); i += op[0].length; continue; }
    let word = '';
    let quoted = false;
    while (i < script.length && !/[\s|;&<>]/.test(script[i])) {
      const c = script[i];
      if (c === "'" || c === '"') {
        const end = c === "'" ? script.indexOf("'", i + 1) : (() => {
          let j = i + 1;
          while (j < script.length && script[j] !== '"') j += script[j] === '\\' ? 2 : 1;
          return j < script.length ? j : -1;
        })();
        if (end < 0) return null;
        word += script.slice(i, end + 1);
        quoted = true;
        i = end + 1;
      } else if (c === '\\' && i + 1 < script.length) {
        word += script.slice(i, i + 2);
        i += 2;
      } else {
        word += c;
        i += 1;
      }
    }
    if (!word) return null;
    tokens.push({ kind: 'word', text: word, quoted });
  }
  return tokens;
}

/** A `sh -c` script, coloured word by word; long ones break after `&&` and `;`. */
function cmdScript(script, ctx) {
  const tokens = cmdTokens(script);
  if (!tokens) return esc(script);
  const long = script.length > 80;
  let position = 0; // 0: before the program, 1: just after it, 2: later
  let broke = false;
  return tokens.map((t) => {
    if (t.kind === 'space') return broke ? '' : esc(t.text);
    broke = false;
    if (t.kind === 'op') {
      position = 0;
      broke = long && (t.text === '&&' || t.text === ';');
      return `<span class="c-op">${esc(t.text)}</span>${broke ? '<br>  ' : ''}`;
    }
    const word = t.text;
    if (t.quoted) { position = position === 0 ? 1 : 2; return `<span class="c-str">${esc(word)}</span>`; }
    if (position === 0) {
      if (CMD_ASSIGN.test(word)) return cmdAssign(word, ctx);
      position = 1;
      return `<span class="c-prog">${cmdIsPath(word) ? cmdPath(word, ctx) : esc(word)}</span>`;
    }
    const sub = position === 1 && /^[a-z][\w:-]*$/i.test(word);
    position = 2;
    if (sub) return `<span class="c-sub">${esc(word)}</span>`;
    if (/^--?[A-Za-z]/.test(word)) return cmdArg(word, ctx);
    return cmdIsPath(word) ? cmdPath(word, ctx) : esc(word);
  }).join('').replace(/^\s+/, '');
}

/** The whole command, as HTML. `ctx` may give `home`. */
function commandHtml(argv, cwd, ctx = {}) {
  const page = typeof last === 'object' && last ? last : {};
  ctx = { home: ctx.home ?? page.home ?? '', cwd: cwd || '', depth: (ctx.depth || 0) + 1 };
  if (!Array.isArray(argv) || !argv.length) return '';
  if (ctx.depth > 4) return esc(cmdJoin(argv));
  const out = [];
  let i = 0;
  const dim = (words) => `<span class="c-dim">${words.map((w) => esc(cmdQuote(w))).join(' ')}</span>`;
  // A script or interpreter that runs `-- <command>`: fold it into a tag.
  const sep = argv.indexOf('--');
  if (sep > 0 && sep < argv.length - 1) {
    const first = cmdBase(argv[0]);
    const script = CMD_INTERPRETERS.test(first) ? argv.slice(1, sep).find((w) => !w.startsWith('-')) : argv[0].includes('/') ? argv[0] : null;
    if (script && !(CMD_SHELLS.test(first) && argv.slice(1, sep).includes('-c'))) {
      const via = `<span class="c-via" title="${esc(cmdJoin(argv.slice(0, sep + 1)))}">via ${esc(cmdBase(script))}</span>`;
      return `${via} ${commandHtml(argv.slice(sep + 1), cwd, ctx)}`;
    }
  }
  // Wrappers, dimmed: env [-flags] [K=V…], xcrun [--sdk X], nice [-n N], time.
  for (;;) {
    const name = cmdBase(argv[i] || '');
    if (name === 'env') {
      const start = i++;
      out.push(dim([argv[start]]));
      while (i < argv.length) {
        const w = argv[i];
        if (w === '--') { out.push(dim([w])); i += 1; break; }
        if (w === '-u' || w === '--unset') { out.push(dim(argv.slice(i, i + 2))); i += 2; continue; }
        if (w.startsWith('-')) { out.push(dim([w])); i += 1; continue; }
        if (CMD_ASSIGN.test(w)) { out.push(cmdAssign(w, ctx)); i += 1; continue; }
        break;
      }
    } else if (name === 'xcrun' || name === 'nice' || name === 'time') {
      const start = i++;
      while (i < argv.length && argv[i].startsWith('-')) {
        i += /^(--?sdk|--?toolchain|-n)$/.test(argv[i]) ? 2 : 1;
      }
      out.push(dim(argv.slice(start, i)));
    } else {
      break;
    }
  }
  const rest = argv.slice(i);
  if (!rest.length) return out.join(' ');
  // sh -c '<script>' [name args…]: the script itself is the command.
  const c = rest.indexOf('-c');
  if (CMD_SHELLS.test(cmdBase(rest[0])) && c > 0 && c + 1 < rest.length && rest.slice(1, c).every((w) => w.startsWith('-'))) {
    out.push(dim(rest.slice(0, c + 1)));
    out.push(`<span class="c-script">${cmdScript(rest[c + 1], ctx)}</span>`);
    if (rest.length > c + 2) out.push(dim(rest.slice(c + 2)));
    return out.join(' ');
  }
  out.push(cmdProgram(rest, ctx));
  return out.join(' ');
}

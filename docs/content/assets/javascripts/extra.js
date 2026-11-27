(() => {
  // The benchmark charts come from the README, where a <picture> picks the
  // light or dark image by the system's preference. Here the reader may pick
  // another scheme with the palette toggle, so the image follows the page's
  // scheme instead.
  const DARK = "(prefers-color-scheme: dark)";

  function charts() {
    const dark = document.body.getAttribute("data-md-color-scheme") === "slate";
    for (const picture of document.querySelectorAll(".md-typeset picture")) {
      const img = picture.querySelector("img");
      const source = picture.querySelector(`source[media="${DARK}"]`);
      if (!img) continue;
      if (source) {
        img.dataset.light = img.getAttribute("src");
        img.dataset.dark = source.getAttribute("srcset");
        source.remove();
      }
      if (img.dataset.light && img.dataset.dark) {
        const wanted = dark ? img.dataset.dark : img.dataset.light;
        if (img.getAttribute("src") !== wanted) img.setAttribute("src", wanted);
        // The lightbox opens the link's image: the same one.
        const link = img.closest("a.glightbox");
        if (link && link.getAttribute("href") !== wanted) link.setAttribute("href", wanted);
      }
    }
  }

  // The home page's install command: typed and erased in turn for each
  // installer in data-commands, with a button that copies the one shown.
  const TYPE_MS = 65;
  const ERASE_MS = 30;
  const HOLD_MS = 2200;
  const GAP_MS = 350;
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

  function show(code, command, length) {
    // The installer's name in the brand colour, the rest as plain text.
    const visible = command.slice(0, length);
    const toolLength = command.indexOf(" ");
    const tool = document.createElement("span");
    tool.className = "rm-install__tool";
    tool.textContent = visible.slice(0, toolLength);
    code.replaceChildren(tool, visible.slice(toolLength));
  }

  function animate(box) {
    const commands = box.dataset.commands.split("|");
    const code = box.querySelector(".rm-install__cmd");
    const button = box.querySelector(".rm-install__copy");
    let index = 0;
    let length = commands[0].length;

    button.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(commands[index]);
        button.classList.add("rm-copied");
        setTimeout(() => button.classList.remove("rm-copied"), 1500);
      } catch {
        // No clipboard (an insecure context, or permission denied).
      }
    });

    function step(phase) {
      if (!box.isConnected) return; // The page changed: stop.
      const command = commands[index];
      if (reducedMotion.matches) {
        index = (index + 1) % commands.length;
        show(code, commands[index], commands[index].length);
        setTimeout(() => step("hold"), HOLD_MS + 800);
        return;
      }
      if (phase === "hold") {
        setTimeout(() => step("erase"), HOLD_MS);
      } else if (phase === "erase") {
        if (length > 0) {
          length -= 1;
          show(code, command, length);
          setTimeout(() => step("erase"), ERASE_MS);
        } else {
          index = (index + 1) % commands.length;
          setTimeout(() => step("type"), GAP_MS);
        }
      } else if (length < command.length) {
        length += 1;
        show(code, command, length);
        setTimeout(() => step("type"), TYPE_MS);
      } else {
        step("hold");
      }
    }

    show(code, commands[0], length);
    step("hold");
  }

  function installs() {
    for (const box of document.querySelectorAll(".rm-install[data-commands]")) {
      if (box.dataset.started) continue;
      box.dataset.started = "1";
      animate(box);
    }
  }

  // The repository's stars on the home page's GitHub button. Fetched once
  // per visit; the page reads fine without them.
  const cache = (key, load) => {
    try {
      const kept = sessionStorage.getItem(key);
      if (kept) return Promise.resolve(JSON.parse(kept));
    } catch {}
    return load().then((value) => {
      try {
        sessionStorage.setItem(key, JSON.stringify(value));
      } catch {}
      return value;
    });
  };
  const json = (url) =>
    fetch(url).then((r) => (r.ok ? r.json() : Promise.reject(r.status)));

  function stars() {
    for (const badge of document.querySelectorAll(".rm-stars[data-repo]")) {
      if (badge.dataset.started) continue;
      badge.dataset.started = "1";
      const repo = badge.dataset.repo;
      cache(`rm-stars-${repo}`, () =>
        json(`https://api.github.com/repos/${repo}`).then((d) => d.stargazers_count),
      )
        .then((count) => {
          badge.textContent = `★ ${Number(count).toLocaleString()}`;
          badge.hidden = false;
        })
        .catch(() => {});
    }
  }

  function apply() {
    charts();
    installs();
    stars();
  }

  // Page changes (instant navigation) replace the content; the toggle
  // changes the body's attribute.
  new MutationObserver(apply).observe(document.body, {
    attributes: true,
    attributeFilter: ["data-md-color-scheme"],
    childList: true,
    subtree: true,
  });
  apply();
})();

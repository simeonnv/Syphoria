(() => {
  if (globalThis.__syphoria) return;
  globalThis.__syphoria = true;

  const api = globalThis.browser ?? globalThis.chrome;
  const SIGNUP_WORDS =
    /sign.?up|regist|create.?account|join|new.?account|onboard/i;
  const USERNAME_HINTS = /user|login|email|mail|account|identifier/i;
  const TEXTUAL = new Set(["text", "email", "tel", ""]);
  const EMAIL_HINTS = /e-?mail/i;
  const USER_HINTS = /user|login|nick|handle|account/i;

  let target = null;
  let reported = null;

  const send = (message) =>
    api.runtime
      .sendMessage(message)
      .catch((error) => console.error("syphoria", error));

  const visible = (el) => {
    const rect = el.getBoundingClientRect();
    return (
      rect.width > 0 &&
      rect.height > 0 &&
      getComputedStyle(el).visibility !== "hidden"
    );
  };

  const scope = (input) => input.form ?? input.closest("form") ?? document;

  const passwords = (root) =>
    [...root.querySelectorAll("input[type=password]")].filter(visible);

  const autocomplete = (input) =>
    (input.getAttribute("autocomplete") ?? "").toLowerCase();

  const textual = (input) =>
    input instanceof HTMLInputElement &&
    TEXTUAL.has(input.type) &&
    !input.readOnly &&
    !input.disabled;

  const usernameField = (root, before) => {
    const candidates = [...root.querySelectorAll("input")].filter(
      (input) => textual(input) && visible(input),
    );
    const hinted = candidates.find((input) =>
      /username|email/.test(autocomplete(input)),
    );
    if (hinted) return hinted;
    const preceding = before
      ? candidates.filter(
          (input) =>
            input.compareDocumentPosition(before) &
            Node.DOCUMENT_POSITION_FOLLOWING,
        )
      : candidates;
    return (
      preceding.at(-1) ??
      candidates.find((input) => USERNAME_HINTS.test(input.name + input.id))
    );
  };

  const describe = (root) => {
    if (root === document) return "";
    const buttons = [...root.querySelectorAll("button, input[type=submit]")]
      .map((b) => `${b.textContent} ${b.value ?? ""}`)
      .join(" ");
    return [
      root.id,
      root.name,
      root.getAttribute("action"),
      root.className,
      buttons,
    ].join(" ");
  };

  const isSignup = (root, fields) =>
    fields.some((p) => autocomplete(p).includes("new-password")) ||
    fields.length >= 2 ||
    SIGNUP_WORDS.test(describe(root)) ||
    SIGNUP_WORDS.test(location.pathname);

  const classify = (input) => {
    if (
      !(input instanceof HTMLInputElement) ||
      input.readOnly ||
      input.disabled
    )
      return null;
    const root = scope(input);
    const fields = passwords(root);
    const isPassword = input.type === "password";
    const isUser =
      textual(input) &&
      (/username|email/.test(autocomplete(input)) ||
        input.type === "email" ||
        (fields.length > 0 && usernameField(root, fields[0]) === input));
    if (!isPassword && !isUser) return null;
    return isSignup(root, fields) ? "signup" : "login";
  };

  const setValue = (input, value) => {
    const setter = Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    ).set;
    input.focus();
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  };

  const emailish = (input) =>
    input.type === "email" ||
    autocomplete(input).includes("email") ||
    EMAIL_HINTS.test(`${input.name} ${input.id} ${input.placeholder}`);

  const userish = (input) =>
    autocomplete(input).includes("username") ||
    USER_HINTS.test(`${input.name} ${input.id} ${input.placeholder}`);

  const fill = (username, email, password) => {
    const anchor = target?.isConnected ? target : passwords(document)[0];
    if (!anchor) return;
    const root = scope(anchor);
    const fields = passwords(root);
    const texts = [...root.querySelectorAll("input")].filter(
      (input) => textual(input) && visible(input),
    );
    const emailField = texts.find(emailish);
    const userField = texts.find((input) => input !== emailField && userish(input));
    const targets = isSignup(root, fields)
      ? fields.filter((p) => !autocomplete(p).includes("current-password"))
      : fields.slice(0, 1);
    if (emailField && userField) {
      setValue(emailField, email || username);
      setValue(userField, username || email);
    } else {
      const single =
        emailField ??
        userField ??
        (anchor.type === "password" ? usernameField(root, fields[0]) : anchor);
      if (single) setValue(single, emailish(single) ? email || username : username || email);
    }
    for (const field of targets) setValue(field, password);
  };

  const capture = (root) => {
    const fields = passwords(root).filter((p) => p.value);
    if (fields.length === 0) return;
    const fresh =
      fields.find((p) => autocomplete(p).includes("new-password")) ??
      fields.at(-1);
    const user = usernameField(root, fields[0]);
    send({ op: "capture", username: user?.value ?? "", password: fresh.value });
  };

  document.addEventListener(
    "focusin",
    (event) => {
      const kind = classify(event.target);
      if (!kind) return;
      target = event.target;
      if (reported === kind) return;
      reported = kind;
      send({ op: "form", kind, focus: true });
    },
    true,
  );

  document.addEventListener(
    "keydown",
    (event) => {
      if (event.key === "Enter" && event.target instanceof HTMLInputElement) {
        capture(scope(event.target));
      }
    },
    true,
  );

  document.addEventListener("submit", (event) => capture(event.target), true);

  document.addEventListener(
    "click",
    (event) => {
      const button = event.target.closest?.("button, input[type=submit]");
      if (button?.type === "submit") capture(scope(button));
    },
    true,
  );

  api.runtime.onMessage.addListener((message) => {
    if (message?.op === "fill") fill(message.username, message.email, message.password);
  });

  const first = passwords(document)[0];
  if (first)
    send({ op: "form", kind: classify(first) ?? "login", focus: false });
  if (window === window.top) send({ op: "loaded" });
})();

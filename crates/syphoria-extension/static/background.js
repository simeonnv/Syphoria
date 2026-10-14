const api = globalThis.browser ?? globalThis.chrome;
const EXPIRY = 5 * 60 * 1000;
const BADGE = "#c4a7e7";
const BADGE_TEXT = "#191724";

const formKey = (tab) => `form:${tab}`;
const captureKey = (tab) => `capture:${tab}`;
const openedKey = (tab) => `opened:${tab}`;

async function read(key) {
  return (await api.storage.session.get(key))[key] ?? null;
}

async function pending(tab) {
  const capture = await read(captureKey(tab));
  if (!capture) return null;
  if (Date.now() - capture.at > EXPIRY) {
    await api.storage.session.remove(captureKey(tab));
    return null;
  }
  return {
    url: capture.url,
    username: capture.username,
    password: capture.password,
  };
}

async function badge(tab) {
  const form = await read(formKey(tab));
  let text = form?.kind === "signup" ? "+" : "";
  if (await pending(tab)) text = "!";
  await api.action.setBadgeBackgroundColor({ tabId: tab, color: BADGE });
  await api.action.setBadgeTextColor?.({ tabId: tab, color: BADGE_TEXT });
  await api.action.setBadgeText({ tabId: tab, text });
}

async function offer(tab, reason) {
  if ((await read(openedKey(tab))) === reason) return;
  await api.storage.session.set({ [openedKey(tab)]: reason });
  try {
    await api.action.openPopup();
  } catch (error) {
    console.debug("syphoria: popup not opened", error);
  }
}

async function context() {
  const [tab] = await api.tabs.query({ active: true, lastFocusedWindow: true });
  if (tab?.id === undefined || !/^https?:/.test(tab.url ?? "")) return null;
  const form = await read(formKey(tab.id));
  return {
    tab: tab.id,
    url: tab.url,
    form: form?.kind ?? null,
    pending: await pending(tab.id),
  };
}

async function trusted(message) {
  switch (message?.op) {
    case "session":
      return (await read("session")) ?? null;
    case "set-session":
      await api.storage.session.set({ session: String(message.session) });
      return null;
    case "clear-session":
      await api.storage.session.remove("session");
      return null;
    case "context":
      return await context();
    case "fill": {
      const form = await read(formKey(message.tab));
      const fill = {
        op: "fill",
        username: message.username,
        email: message.email,
        password: message.password,
      };
      const options = form?.frame === undefined ? {} : { frameId: form.frame };
      await api.tabs.sendMessage(message.tab, fill, options);
      return null;
    }
    case "discard":
      await api.storage.session.remove(captureKey(message.tab));
      await badge(message.tab);
      return null;
    default:
      return null;
  }
}

async function content(message, sender) {
  const tab = sender.tab?.id;
  if (tab === undefined) return null;
  switch (message?.op) {
    case "form": {
      const kind = message.kind === "signup" ? "signup" : "login";
      const known = await read(formKey(tab));
      if (!(known?.kind === "signup" && kind === "login" && !message.focus)) {
        await api.storage.session.set({
          [formKey(tab)]: { kind, frame: sender.frameId },
        });
      }
      await badge(tab);
      if (message.focus) await offer(tab, `form:${sender.tab.url}`);
      return null;
    }
    case "capture":
      await api.storage.session.set({
        [captureKey(tab)]: {
          url: sender.url,
          username: String(message.username ?? ""),
          password: String(message.password ?? ""),
          at: Date.now(),
        },
      });
      await badge(tab);
      return null;
    case "loaded":
      if (await pending(tab)) {
        await badge(tab);
        await offer(tab, `save:${sender.tab.url}`);
      }
      return null;
    default:
      return null;
  }
}

function handle(message, sender) {
  const own =
    sender.id === api.runtime.id &&
    sender.url?.startsWith(api.runtime.getURL(""));
  return own ? trusted(message) : content(message, sender);
}

api.runtime.onMessage.addListener((message, sender, respond) => {
  handle(message, sender).then(respond, (error) => {
    console.error("syphoria", error);
    respond(null);
  });
  return true;
});

api.tabs.onUpdated.addListener((tab, change) => {
  if (change.status !== "loading") return;
  api.storage.session
    .remove([formKey(tab), openedKey(tab)])
    .then(() => badge(tab))
    .catch((error) => console.error("syphoria", error));
});

api.tabs.onRemoved.addListener((tab) => {
  api.storage.session
    .remove([formKey(tab), openedKey(tab), captureKey(tab)])
    .catch((error) => console.error("syphoria", error));
});

(() => {
    let storage;
    let frame;
    let token;
    try {
        try { storage = window.localStorage; } catch (_) {}
        let raw = storage && storage.getItem('token');
        if (!raw) {
            frame = document.createElement('iframe');
            frame.style.display = 'none';
            document.documentElement.appendChild(frame);
            storage = frame.contentWindow.localStorage;
            raw = storage.getItem('token');
        }
        token = JSON.parse(raw);
    } catch (_) {
        return null;
    } finally {
        if (frame) frame.remove();
    }
    if (typeof token !== 'string' || !token) return null;

    // Storage survives tinyclient's logout request. Never hand that revoked token back
    // to chatd just because the web client has not noticed the logout yet.
    const previous = window.__tinyclientDiscordSession;
    if (previous && previous.token === token) {
        return previous.valid ? JSON.stringify({token}) : null;
    }
    const check = {token, valid: false};
    window.__tinyclientDiscordSession = check;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 10000);
    fetch('https://discord.com/api/v9/users/@me', {
        headers: {Authorization: token},
        signal: controller.signal,
        cache: 'no-store',
    }).then(response => {
        if (window.__tinyclientDiscordSession !== check) return;
        if (response.ok) {
            check.valid = true;
        } else if (response.status === 401) {
            // Reset this origin's stale browser login, then let Discord show its form.
            if (JSON.parse(storage.getItem('token')) !== token) {
                delete window.__tinyclientDiscordSession;
                return;
            }
            storage.clear();
            window.location.replace('https://discord.com/login');
        } else {
            delete window.__tinyclientDiscordSession;
        }
    }).catch(() => {
        if (window.__tinyclientDiscordSession === check) {
            delete window.__tinyclientDiscordSession;
        }
    }).finally(() => clearTimeout(timeout));
    return null;
})()

(() => {
    const withStorage = fn => {
        let frame;
        try {
            let storage;
            try { storage = window.localStorage; } catch (_) {}
            if (storage) {
                try {
                    const out = fn(storage);
                    if (out !== null && out !== undefined) return out;
                } catch (_) {}
            }
            frame = document.createElement('iframe');
            frame.style.display = 'none';
            document.documentElement.appendChild(frame);
            return fn(frame.contentWindow.localStorage);
        } catch (_) {
            return null;
        } finally {
            if (frame) frame.remove();
        }
    };

    const parseToken = raw => {
        if (!raw) return null;
        try {
            const parsed = JSON.parse(raw);
            return typeof parsed === 'string' && parsed ? parsed : null;
        } catch (_) {
            return null;
        }
    };

    const token = parseToken(withStorage(localStorage => localStorage.getItem('token')));
    if (!token) return null;

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
            const cleared = withStorage(storage => {
                const current = parseToken(storage.getItem('token'));
                if (!current) return null;
                if (current !== token) return false;
                storage.clear();
                return true;
            });
            if (cleared) {
                window.location.replace('https://discord.com/login');
            } else {
                delete window.__tinyclientDiscordSession;
            }
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

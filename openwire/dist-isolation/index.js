const ALLOWED_COMMANDS = new Set([
    'send', 'send_file', 'list_contacts', 'list_identities', 'select_identity',
    'delete_identity', 'generate_identity', 'add_contact',
    'delete_contact', 'delete_message',
    'request_file_download', 'set_download_dir', 'get_download_dir',
    'load_messages', 'get_identity_qr_data',
    'check_core_ready',
    'is_keyring_available',
    'get_nodes_config', 'save_nodes_config', 'reset_nodes_config',
    'list_sent_files', 'delete_sent_file',
    'plugin:window|set_content_protected'
]);

const ALLOWED_PLUGIN_PREFIXES = ['plugin:store|', 'plugin:opener|', 'plugin:dialog|', 'plugin:event|', 'plugin:path|'];

const SENSITIVE_COMMANDS = new Set([
    'send', 'send_file', 'delete_identity', 'select_identity',
    'generate_identity', 'add_contact', 'delete_contact',
    'request_file_download', 'delete_sent_file',
    'set_download_dir'
]);

const RATE_LIMITS = {
    send: { maxCalls: 10, windowMs: 60000 },
    send_file: { maxCalls: 10, windowMs: 60000 },
    generate_identity: { maxCalls: 3, windowMs: 60000 },
    add_contact: { maxCalls: 20, windowMs: 60000 },
    delete_contact: { maxCalls: 10, windowMs: 60000 },
    delete_identity: { maxCalls: 5, windowMs: 60000 },
    select_identity: { maxCalls: 30, windowMs: 60000 },
    request_file_download: { maxCalls: 20, windowMs: 60000 },
    set_download_dir: { maxCalls: 5, windowMs: 60000 },
    list_contacts: { maxCalls: 60, windowMs: 60000 },
    list_identities: { maxCalls: 60, windowMs: 60000 },
    get_download_dir: { maxCalls: 60, windowMs: 60000 },
    load_messages: { maxCalls: 60, windowMs: 60000 },
    get_identity_qr_data: { maxCalls: 30, windowMs: 60000 },
    check_core_ready: { maxCalls: 300, windowMs: 60000 },
    list_sent_files: { maxCalls: 60, windowMs: 60000 },
    delete_sent_file: { maxCalls: 10, windowMs: 60000 },
};

const DEFAULT_RATE_LIMIT = { maxCalls: 30, windowMs: 60000 };

const callRecords = new Map();

function checkAndRecordCall(cmd) {
    const now = Date.now();
    const limit = RATE_LIMITS[cmd] || DEFAULT_RATE_LIMIT;
    const { maxCalls, windowMs } = limit;

    let timestamps = callRecords.get(cmd) || [];
    const cutoff = now - windowMs;
    const recentCalls = timestamps.filter(t => t > cutoff);

    if (recentCalls.length >= maxCalls) {
        const oldestCall = recentCalls[0];
        const resetTime = Math.ceil((oldestCall + windowMs - now) / 1000);
        return { allowed: false, resetAfter: resetTime };
    }

    recentCalls.push(now);
    callRecords.set(cmd, recentCalls);
    return { allowed: true };
}

setInterval(() => {
    const now = Date.now();
    for (const [cmd, ts] of callRecords.entries()) {
        const limit = RATE_LIMITS[cmd] || DEFAULT_RATE_LIMIT;
        const valid = ts.filter(t => t > now - limit.windowMs);
        valid.length ? callRecords.set(cmd, valid) : callRecords.delete(cmd);
    }
}, 60000);

function sanitize(payload) {
    if (!payload || typeof payload !== 'object') return payload;
    const result = {};
    for (const [key, val] of Object.entries(payload)) {
        if (typeof val === 'string') {
            if ((key === 'pubkeyHex' || key === 'identityId') && val.length > 16) {
                result[key] = val.slice(0, 8) + '***' + val.slice(-4);
            } else if (key === 'message' && val.length > 50) {
                result[key] = val.slice(0, 50) + '...';
            } else {
                result[key] = val;
            }
        } else {
            result[key] = val;
        }
    }
    return result;
}

function logError(msg, payload) {
    console.error('[ISOLATION]', msg, sanitize(payload), `visibility:${document.visibilityState}`);
}

function logInfo(cmd, payload) {
    console.log('[ISOLATION]', cmd, sanitize(payload));
}

function isHex(str) {
    return typeof str === 'string' && /^[0-9a-fA-F]+$/.test(str);
}

function requiredString(val, name, maxLen) {
    if (typeof val !== 'string' || val.length === 0) return `${name} required`;
    if (maxLen && val.length > maxLen) return `${name} exceeds limit (${maxLen})`;
    return null;
}

const VALIDATORS = {
    send: (p) => {
        let err = requiredString(p.mldsaPubkeyHex, 'mldsaPubkeyHex');
        if (err) return err;
        if (!isHex(p.mldsaPubkeyHex)) return 'mldsaPubkeyHex must be hex';
        return requiredString(p.message, 'message', 65536);
    },
    send_file: (p) => {
        let err = requiredString(p.mldsaPubkeyHex, 'mldsaPubkeyHex');
        if (err) return err;
        if (!isHex(p.mldsaPubkeyHex)) return 'mldsaPubkeyHex must be hex';
        err = requiredString(p.filePath, 'filePath', 4096);
        if (err) return err;
        return null;
    },
    add_contact: (p) => {
        let err = requiredString(p.mldsaPubkeyHex, 'mldsaPubkeyHex');
        if (err) return err;
        if (!isHex(p.mldsaPubkeyHex)) return 'mldsaPubkeyHex must be hex';
        if (p.name !== undefined && p.name !== null && typeof p.name !== 'string') return 'name must be string';
        if (typeof p.name === 'string' && p.name.length > 256) return 'name exceeds limit (256)';
        return null;
    },
    delete_contact: (p) => {
        let err = requiredString(p.mldsaPubkeyHex, 'mldsaPubkeyHex');
        if (err) return err;
        if (!isHex(p.mldsaPubkeyHex)) return 'mldsaPubkeyHex must be hex';
        return null;
    },
    select_identity: (p) => {
        let err = requiredString(p.identityId, 'identityId');
        if (err) return err;
        if (!isHex(p.identityId)) return 'identityId must be hex';
        return null;
    },
    delete_identity: (p) => {
        let err = requiredString(p.identityId, 'identityId');
        if (err) return err;
        if (!isHex(p.identityId)) return 'identityId must be hex';
        return null;
    },
    request_file_download: (p) => {
        let err = requiredString(p.senderMldsaPubkeyHex, 'senderMldsaPubkeyHex');
        if (err) return err;
        if (!isHex(p.senderMldsaPubkeyHex)) return 'senderMldsaPubkeyHex must be hex';
        err = requiredString(p.fileIdHex, 'fileIdHex');
        if (err) return err;
        if (!isHex(p.fileIdHex)) return 'fileIdHex must be hex';
        return null;
    },
    set_download_dir: (p) => requiredString(p.path, 'path', 4096),
    list_contacts: () => null,
    list_identities: () => null,
    generate_identity: () => null,
    get_download_dir: () => null,
    load_messages: (p) => {
        let err = requiredString(p.mldsaPubkeyHex, 'mldsaPubkeyHex');
        if (err) return err;
        if (!isHex(p.mldsaPubkeyHex)) return 'mldsaPubkeyHex must be hex';
        return null;
    },
    get_identity_qr_data: () => null,
    check_core_ready: () => null,
    is_keyring_available: () => null,
    get_nodes_config: () => null,
    reset_nodes_config: () => null,
    list_sent_files: () => null,
    delete_sent_file: (p) => {
        let err = requiredString(p.fileHashHex, 'fileHashHex');
        if (err) return err;
        if (!isHex(p.fileHashHex)) return 'fileHashHex must be hex';
        if (p.fileHashHex.length !== 64) return 'fileHashHex must be 64 hex chars';
        return null;
    },
    save_nodes_config: (p) => {
        if (!Array.isArray(p.relayNodes)) return 'relayNodes must be an array';
        if (!Array.isArray(p.bootstrapNodes)) return 'bootstrapNodes must be an array';
        for (const node of p.relayNodes) {
            if (!Array.isArray(node) || node.length !== 2 || typeof node[0] !== 'string' || typeof node[1] !== 'string') {
                return 'Each relay node must be [peer_id, multiaddr]';
            }
            if (node[0].length > 256) return 'relay node peer_id too long';
            if (node[1].length > 1024) return 'relay node multiaddr too long';
        }
        for (const node of p.bootstrapNodes) {
            if (!Array.isArray(node) || node.length !== 2 || typeof node[0] !== 'string' || typeof node[1] !== 'string') {
                return 'Each bootstrap node must be [peer_id, multiaddr]';
            }
            if (node[0].length > 256) return 'bootstrap node peer_id too long';
            if (node[1].length > 1024) return 'bootstrap node multiaddr too long';
        }
        if (p.relayNodes.length > 50) return 'Too many relay nodes (max 50)';
        if (p.bootstrapNodes.length > 50) return 'Too many bootstrap nodes (max 50)';
        return null;
    },
};

window.__TAURI_ISOLATION_HOOK__ = (payload) => {
    try {
        if (!payload || typeof payload !== 'object') {
            logError('Invalid payload', payload);
            return null;
        }

        const { cmd } = payload;
        if (typeof cmd !== 'string' || !cmd) {
            logError('Missing cmd field', payload);
            return null;
        }

        const isPlugin = cmd.startsWith('plugin:');
        const isAllowed = ALLOWED_COMMANDS.has(cmd) ||
            (isPlugin && ALLOWED_PLUGIN_PREFIXES.some(p => cmd.startsWith(p)));

        if (!isAllowed) {
            logError(`Unknown command: ${cmd}`, payload);
            return null;
        }

        if (!isPlugin && document.visibilityState === 'hidden' && SENSITIVE_COMMANDS.has(cmd)) {
            logError(`Sensitive command blocked (background): ${cmd}`, payload);
            return null;
        }

        if (!isPlugin) {
            const rateCheck = checkAndRecordCall(cmd);
            if (!rateCheck.allowed) {
                logError(`Rate limit exceeded: ${cmd}`, payload);
                return null;
            }
        }

        const args = payload.payload;

        const validator = VALIDATORS[cmd];
        if (validator) {
            const err = validator(args);
            if (err) {
                logError(`Validation failed: ${err}`, payload);
                return null;
            }
        }

        logInfo(cmd, payload);
        return payload;

    } catch (err) {
        console.error('[ISOLATION]', err.message);
        return null;
    }
};
const ALLOWED_COMMANDS = new Set([
    'send', 'send_file', 'list_contacts', 'list_identities', 'select_identity',
    'delete_identity', 'generate_identity', 'add_contact', 'discover_contact',
    'delete_contact', 'delete_message',
    'request_file_download',
    'load_messages', 'get_identity_qr_data',
    'check_core_ready',
    'is_keyring_available',
    'get_nodes_config', 'save_nodes_config', 'reset_nodes_config',
    'list_sent_files', 'delete_sent_file',
    'get_network_status',
    'export_routing_table', 'import_routing_table', 'read_text_file', 'set_paid_network',
    'set_relay_role', 'dial_peer', 'get_version', 'on_foreground',
    'plugin:window|set_content_protected'
]);

const ALLOWED_PLUGIN_PREFIXES = ['plugin:store|', 'plugin:dialog|', 'plugin:event|', 'plugin:path|'];

const SENSITIVE_COMMANDS = new Set([
    'send', 'send_file', 'delete_identity', 'select_identity',
    'generate_identity', 'add_contact', 'delete_contact', 'delete_message',
    'request_file_download', 'delete_sent_file',
    'save_nodes_config', 'reset_nodes_config',
    'discover_contact', 'export_routing_table', 'set_paid_network'
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
    list_contacts: { maxCalls: 60, windowMs: 60000 },
    list_identities: { maxCalls: 60, windowMs: 60000 },
    load_messages: { maxCalls: 60, windowMs: 60000 },
    get_identity_qr_data: { maxCalls: 30, windowMs: 60000 },
    check_core_ready: { maxCalls: 300, windowMs: 60000 },
    list_sent_files: { maxCalls: 60, windowMs: 60000 },
    delete_sent_file: { maxCalls: 10, windowMs: 60000 },
    get_network_status: { maxCalls: 60, windowMs: 60000 },
    export_routing_table: { maxCalls: 10, windowMs: 60000 },
    import_routing_table: { maxCalls: 10, windowMs: 60000 },
    read_text_file: { maxCalls: 30, windowMs: 60000 },
    set_paid_network: { maxCalls: 30, windowMs: 60000 },
    set_relay_role: { maxCalls: 30, windowMs: 60000 },
    dial_peer: { maxCalls: 10, windowMs: 60000 },
    on_foreground: { maxCalls: 60, windowMs: 60000 },
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
            if ((key === 'pubkeyHex' || key === 'identityId' || key === 'mldsaPubkeyHex') && val.length > 16) {
                result[key] = val.slice(0, 8) + '***' + val.slice(-4);
            } else if (key === 'message' && val.length > 50) {
                result[key] = val.slice(0, 50) + '...';
            } else if (['filePath', 'savePath', 'src', 'dst', 'path', 'addr', 'fileHashHex'].includes(key) && val.length > 80) {
                result[key] = val.slice(0, 80) + '...';
            } else if (key === 'data' && val.length > 200) {
                result[key] = val.slice(0, 200) + '...';
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
        if (typeof p.message !== 'string' || p.message.length === 0) return 'message required';
        if (new TextEncoder().encode(p.message).length > 65536) return 'message exceeds limit (65536 bytes)';
        return null;
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
        err = requiredString(p.fileHashHex, 'fileHashHex');
        if (err) return err;
        if (!isHex(p.fileHashHex)) return 'fileHashHex must be hex';
        if (p.fileHashHex.length !== 64) return 'fileHashHex must be 64 hex chars';
        return null;
    },
    list_contacts: () => null,
    list_identities: () => null,
    generate_identity: () => null,
    load_messages: (p) => {
        let err = requiredString(p.mldsaPubkeyHex, 'mldsaPubkeyHex');
        if (err) return err;
        if (!isHex(p.mldsaPubkeyHex)) return 'mldsaPubkeyHex must be hex';
        // 与 Rust 端 openwire_core::storage::message::MAX_MESSAGE_PAGE_SIZE (200) 保持一致
if (p.limit !== undefined && (typeof p.limit !== 'number' || p.limit < 0 || p.limit > 200)) return 'limit must be between 0 and 200';
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
    get_network_status: () => null,
    export_routing_table: (p) => {
        if (typeof p.savePath !== 'string' || p.savePath.length === 0) return 'savePath required';
        if (p.savePath.length > 4096) return 'savePath too long';
        if (/\.\./.test(p.savePath)) return 'path traversal not allowed';
        return null;
    },
    import_routing_table: (p) => {
        if (typeof p.data !== 'string' || p.data.length === 0) return 'data required';
        if (p.data.length > 10485760) return 'data exceeds 10MB limit';
        return null;
    },
    read_text_file: (p) => {
        if (typeof p.path !== 'string' || p.path.length === 0) return 'path required';
        if (p.path.length > 4096) return 'path too long';
        if (/\.\./.test(p.path)) return 'path traversal not allowed';
        return null;
    },
    set_paid_network: (p) => {
        if (!['free', 'paid', 'disabled'].includes(p.mode)) return 'mode must be free/paid/disabled';
        return null;
    },
    set_relay_role: (p) => {
        if (!['server', 'client', 'off'].includes(p.role)) return 'role must be server/client/off';
        return null;
    },
    dial_peer: (p) => {
        let err = requiredString(p.peerId, 'peerId', 128);
        if (err) return err;
        err = requiredString(p.addr, 'addr', 1024);
        if (err) return err;
        if (/\.\./.test(p.addr)) return 'invalid addr';
        return null;
    },
    discover_contact: (p) => {
        let err = requiredString(p.mldsaPubkeyHex, 'mldsaPubkeyHex');
        if (err) return err;
        if (!isHex(p.mldsaPubkeyHex)) return 'mldsaPubkeyHex must be hex';
        return null;
    },
    get_version: () => null,
    on_foreground: () => null,
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
window.__TAURI_ISOLATION_HOOK__ = (payload) => {
    // 不需要验证或修改任何内容，仅输出钩子中的内容
    console.log('hook', payload);
    return payload;
};
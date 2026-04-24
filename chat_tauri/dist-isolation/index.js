window.__TAURI_ISOLATION_HOOK__ = (payload) => {
    // 不需要验证或修改任何内容，仅输出钩子中的内容
    console.log('hook', payload);
    //todo :check cmd
    if (document.visibilityState === 'hidden') {//当页面隐藏
        switch (payload.cmd) {
            case 'send':
            //拦截部分应当由用户发出的指令，并警告可能前端被攻击
        }

    }

    return payload;
};
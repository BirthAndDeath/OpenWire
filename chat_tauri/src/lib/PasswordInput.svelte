<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";

    interface Props {
        /** 密码设置成功后的回调 */
        onSuccess?: () => void;
    }

    let { onSuccess }: Props = $props();

    // 密码输入状态
    let password = $state("");
    let confirmPassword = $state("");
    let showPassword = $state(false);
    let isSetting = $state(false);
    let error = $state("");
    let success = $state("");

    // 密码强度检查
    let strength = $derived.by(() => {
        if (password.length === 0) return { level: 0, label: "", color: "" };
        if (password.length < 8)
            return { level: 1, label: "弱", color: "#ef4444" };
        if (password.length < 12)
            return { level: 2, label: "中", color: "#f59e0b" };
        return { level: 3, label: "强", color: "#10b981" };
    });

    // 密码是否匹配
    let passwordsMatch = $derived(
        password.length > 0 &&
            confirmPassword.length > 0 &&
            password === confirmPassword,
    );

    // 重置状态
    function reset() {
        password = "";
        confirmPassword = "";
        showPassword = false;
        error = "";
        success = "";
        isSetting = false;
    }

    // 提交密码
    async function handleSubmit() {
        error = "";
        success = "";

        if (!password) {
            error = "请输入密码";
            return;
        }
        if (password.length < 8) {
            error = "密码长度至少 8 位";
            return;
        }
        if (password !== confirmPassword) {
            error = "两次输入的密码不一致";
            return;
        }

        isSetting = true;
        try {
            await invoke("set_password", { password });
            success = "密码已设置成功";
            setTimeout(() => {
                onSuccess?.();
                reset();
            }, 1500);
        } catch (e) {
            error = `设置密码失败：${e}`;
        } finally {
            isSetting = false;
        }
    }

    // 清除密码
    async function handleClear() {
        error = "";
        success = "";
        try {
            await invoke("set_password", { password: "" });
            success = "密码已清除";
            reset();
        } catch (e) {
            error = `清除密码失败：${e}`;
        }
    }
</script>

<div class="password-input">
    <div class="password-field">
        <label for="password-input">设置密码</label>
        <div class="input-wrapper">
            <input
                id="password-input"
                type={showPassword ? "text" : "password"}
                bind:value={password}
                placeholder="输入密码（至少 8 位）"
                disabled={isSetting}
                autocomplete="new-password"
            />
            <button
                class="toggle-visibility"
                onclick={() => (showPassword = !showPassword)}
                type="button"
                aria-label={showPassword ? "隐藏密码" : "显示密码"}
            >
                {showPassword ? "🙈" : "👁"}
            </button>
        </div>
        {#if password.length > 0}
            <div class="strength-bar">
                <div
                    class="strength-fill"
                    style="width: {strength.level *
                        33}%; background: {strength.color};"
                ></div>
            </div>
            <span class="strength-label" style="color: {strength.color};">
                密码强度：{strength.label}
            </span>
        {/if}
    </div>

    <div class="password-field">
        <label for="confirm-password-input">确认密码</label>
        <input
            id="confirm-password-input"
            type={showPassword ? "text" : "password"}
            bind:value={confirmPassword}
            placeholder="再次输入密码"
            disabled={isSetting}
            autocomplete="new-password"
        />
        {#if confirmPassword.length > 0}
            <span
                class="match-hint"
                class:match={passwordsMatch}
                class:mismatch={!passwordsMatch}
            >
                {passwordsMatch ? "✓ 密码匹配" : "✗ 密码不匹配"}
            </span>
        {/if}
    </div>

    {#if error}
        <div class="message error">{error}</div>
    {/if}
    {#if success}
        <div class="message success">{success}</div>
    {/if}

    <div class="actions">
        <button
            class="btn primary"
            onclick={handleSubmit}
            disabled={isSetting || !password || !passwordsMatch}
        >
            {isSetting ? "设置中..." : "设置密码"}
        </button>
        <button
            class="btn secondary"
            onclick={handleClear}
            disabled={isSetting}
        >
            清除密码
        </button>
    </div>

    <div class="warning-note">
        ⚠️ 如果忘记密码，将无法找回身份，无法启动应用。请妥善保管密码。
    </div>
</div>

<style>
    .password-input {
        display: flex;
        flex-direction: column;
        gap: 16px;
    }

    .password-field {
        display: flex;
        flex-direction: column;
        gap: 6px;
    }

    .password-field label {
        font-size: 13px;
        font-weight: 500;
        color: var(--text-primary, #fafafa);
    }

    .input-wrapper {
        position: relative;
        display: flex;
        align-items: center;
    }

    .input-wrapper input {
        flex: 1;
        padding-right: 40px;
    }

    .password-field input {
        background: var(--bg-secondary, #0a0a0a);
        border: 1px solid var(--border-color, #2a2a2a);
        border-radius: 6px;
        padding: 10px 12px;
        color: var(--text-primary, #fafafa);
        font-size: 14px;
        font-family: monospace;
        transition: border-color 0.2s;
        width: 100%;
        box-sizing: border-box;
    }

    .password-field input:focus {
        outline: none;
        border-color: #3b82f6;
        box-shadow: 0 0 0 2px rgba(59, 130, 246, 0.2);
    }

    .password-field input:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .toggle-visibility {
        position: absolute;
        right: 8px;
        background: transparent;
        border: none;
        cursor: pointer;
        font-size: 16px;
        padding: 4px;
        color: var(--text-secondary, #737373);
        transition: color 0.2s;
    }

    .toggle-visibility:hover {
        color: var(--text-primary, #fafafa);
    }

    .strength-bar {
        height: 4px;
        background: var(--bg-secondary, #0a0a0a);
        border-radius: 2px;
        overflow: hidden;
    }

    .strength-fill {
        height: 100%;
        border-radius: 2px;
        transition:
            width 0.3s,
            background 0.3s;
    }

    .strength-label {
        font-size: 11px;
        transition: color 0.3s;
    }

    .match-hint {
        font-size: 12px;
    }

    .match-hint.match {
        color: #10b981;
    }

    .match-hint.mismatch {
        color: #ef4444;
    }

    .message {
        padding: 8px 12px;
        border-radius: 6px;
        font-size: 13px;
    }

    .message.error {
        background: rgba(239, 68, 68, 0.1);
        border: 1px solid rgba(239, 68, 68, 0.3);
        color: #ef4444;
    }

    .message.success {
        background: rgba(16, 185, 129, 0.1);
        border: 1px solid rgba(16, 185, 129, 0.3);
        color: #10b981;
    }

    .actions {
        display: flex;
        gap: 12px;
    }

    .btn {
        padding: 10px 20px;
        border-radius: 6px;
        border: 1px solid var(--border-color, #2a2a2a);
        font-size: 14px;
        cursor: pointer;
        transition: all 0.2s;
        font-weight: 500;
    }

    .btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .btn.primary {
        background: #3b82f6;
        border-color: #3b82f6;
        color: white;
        flex: 1;
    }

    .btn.primary:hover:not(:disabled) {
        background: #2563eb;
    }

    .btn.secondary {
        background: transparent;
        color: var(--text-primary, #fafafa);
    }

    .btn.secondary:hover:not(:disabled) {
        background: var(--bg-tertiary, #1a1a1a);
        border-color: #ef4444;
        color: #ef4444;
    }

    .warning-note {
        font-size: 12px;
        color: #f59e0b;
        background: rgba(245, 158, 11, 0.1);
        border: 1px solid rgba(245, 158, 11, 0.2);
        padding: 8px 12px;
        border-radius: 6px;
        line-height: 1.5;
    }
</style>

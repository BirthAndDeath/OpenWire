<script lang="ts">
    import { _ } from "svelte-i18n";
</script>

<main>
    <div class="hero">
        <a
            href="https://github.com/BirthAndDeath/chat"
            target="_blank"
            class="logo-link"
        >
            <img src="/logo.svg" alt="Chat Logo" />
        </a>
        <h1>{$_("secure_chat")}</h1>
        <p class="tagline">{$_("tagline1")}</p>
        <p class="tagline">{$_("tagline2")}</p>
    </div>

    <div class="links">
        <a href="/" class="btn secondary">{$_("home")}</a>
        <a
            href="https://github.com/BirthAndDeath/chat"
            target="_blank"
            class="btn primary"
        >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                <path
                    d="M12 0c-6.626 0-12 5.373-12 12 0 5.302 3.438 9.8 8.207 11.387.599.111.793-.261.793-.577v-2.234c-3.338.726-4.033-1.416-4.033-1.416-.546-1.387-1.333-1.756-1.333-1.756-1.089-.745.083-.729.083-.729 1.205.084 1.839 1.237 1.839 1.237 1.07 1.834 2.807 1.304 3.492.997.107-.775.418-1.305.762-1.604-2.665-.305-5.467-1.334-5.467-5.931 0-1.311.469-2.381 1.236-3.221-.124-.303-.535-1.524.117-3.176 0 0 1.008-.322 3.301 1.23.957-.266 1.983-.399 3.003-.404 1.02.005 2.047.138 3.006.404 2.291-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.874.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.807 5.624-5.479 5.921.43.372.823 1.102.823 2.222v3.293c0 .319.192.694.801.576 4.765-1.589 8.199-6.086 8.199-11.386 0-6.627-5.373-12-12-12z"
                />
            </svg>
            GitHub
        </a>
    </div>

    <div class="support">
        <a
            href="https://space.bilibili.com/3494362084280927/"
            target="_blank"
            rel="noopener"
            class="bili-link"
        >
            <svg
                width="20"
                height="20"
                viewBox="0 0 512 512"
                fill="currentColor"
            >
                <path
                    d="M488.6 104.1c16.7 18.1 24.4 39.7 23.3 65.7v202.4c-.4 26.4-9.2 48.1-26.5 65.1-17.2 17-39.1 25.9-65.5 26.7H92.02c-26.45-.8-48.21-9.8-65.28-27.2C9.682 419.4.767 397.5.968 371.6V169.2c-1.153-26.4 7.645-48.4 26.392-66.1 18.75-17.7 40.61-26.5 65.58-26.4h35.7l21.2-40.95c6.5-11.93 16.6-17.88 30.3-17.88 13.8 0 23.9 5.95 30.3 17.88l21.2 40.95h146.4l21.2-40.95c6.5-11.93 16.6-17.88 30.3-17.88 13.8 0 23.9 5.95 30.3 17.88l21.2 40.95h35.7c25-.1 46.8 8.7 65.5 26.4zM96 224c-17.7 0-32 14.3-32 32s14.3 32 32 32 32-14.3 32-32-14.3-32-32-32zm320 0c-17.7 0-32 14.3-32 32s14.3 32 32 32 32-14.3 32-32-14.3-32-32-32z"
                />
            </svg>
            Bilibili
        </a>
        <span>Consider supporting me 🥰</span>
    </div>

    <p class="desc">{$_("details")}</p>
</main>

<style>
    :global(body) {
        margin: 0;
        background: #0a0a0a;
        color: #fafafa;
        font-family:
            system-ui,
            -apple-system,
            sans-serif;
    }

    main {
        min-height: 100vh;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        padding: 2rem;
        box-sizing: border-box;
    }

    .hero {
        text-align: center;
        margin-bottom: 2rem;
    }

    .logo-link {
        display: inline-block;
        transition: transform 0.3s ease;
    }

    .logo-link:hover {
        transform: scale(1.05);
    }

    .logo-link img {
        height: 120px;
        width: 120px;
        filter: drop-shadow(0 0 20px rgba(129, 129, 131, 0.3));
        transition: filter 0.3s;
    }

    .logo-link:hover img {
        filter: drop-shadow(0 0 40px rgba(129, 129, 131, 0.6));
    }

    h1 {
        font-size: 2.5rem;
        font-weight: 700;
        margin: 1rem 0 0.5rem;
        background: linear-gradient(135deg, #fafafa 0%, #818183 100%);
        -webkit-background-clip: text;
        background-clip: text;
        -webkit-text-fill-color: transparent;
    }

    .tagline {
        color: #737373;
        font-size: 1rem;
        margin: 0;
    }

    .links {
        display: flex;
        gap: 1rem;
        margin-bottom: 2rem;
    }

    .btn {
        display: inline-flex;
        align-items: center;
        gap: 0.5rem;
        padding: 0.75rem 1.5rem;
        border-radius: 8px;
        font-weight: 500;
        text-decoration: none;
        transition: all 0.2s;
    }

    .btn.primary {
        background: #3b82f6;
        color: white;
    }

    .btn.primary:hover {
        background: #2563eb;
        transform: translateY(-2px);
    }

    .btn.secondary {
        background: #1a1a1a;
        color: #fafafa;
        border: 1px solid #2a2a2a;
    }

    .btn.secondary:hover {
        background: #252525;
        border-color: #3b82f6;
    }

    .support {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        padding: 1rem 1.5rem;
        background: #141414;
        border-radius: 12px;
        border: 1px solid #2a2a2a;
    }

    .bili-link {
        display: inline-flex;
        align-items: center;
        gap: 0.5rem;
        color: #00aeec;
        text-decoration: none;
        font-weight: 500;
        transition: opacity 0.2s;
    }

    .bili-link:hover {
        opacity: 0.8;
    }

    .support span {
        color: #737373;
        font-size: 0.9rem;
    }

    .desc {
        max-width: 400px;
        text-align: center;
        color: #525252;
        font-size: 0.875rem;
        line-height: 1.6;
        margin-top: 2rem;
    }
</style>

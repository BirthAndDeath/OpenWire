<script lang="ts">
    import { _ } from "svelte-i18n";
</script>

<main>
    <div class="row">
        <h1>Welcome to Chat</h1>
        <a href="/" title={$_("home")}>{$_("home")}</a>
        <a href="https://github.com/BirthAndDeath/chat" target="_blank">
            <img src="/logo.svg" class="logo chat" alt="Chat Logo" />
        </a>
    </div>
    <p>
        <a
            href="https://space.bilibili.com/3494362084280927/"
            target="_blank"
            rel="noopener noreferrer"
            title="Bilibili account"
        >
            Bilibili Account
        </a> Consider supporting me 🥰
    </p>
    <p>Click the logo to go to the GitHub page.</p>
    <p>{$_("details")}</p>
</main>

<style>
    .logo {
        height: 6em;
        padding: 1.5em;
        will-change: filter;
        transition: 0.75s;
    }
    .logo.chat:hover {
        filter: drop-shadow(0 0 2em #818183);
    }
    a {
        font-weight: 500;
        color: #646cff;
        text-decoration: inherit;
    }

    a:hover {
        color: #535bf2;
    }
    h1 {
        text-align: center;
    }
    @media (prefers-color-scheme: dark) {
        :root {
            color: #f6f6f6;
            background-color: #2f2f2f;
        }

        a:hover {
            color: #24c8db;
        }
    }
</style>

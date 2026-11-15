// Where the site and its login provider live. Read once, in the browser.
const local = ["127.0.0.1", "localhost"].includes(location.hostname);

export const CONFIG = {
  issuer: local
    ? "http://127.0.0.1:8180/realms/players"
    : "https://auth.factoriosurvivalworld.com/realms/players",
  clientId: "site",
  siteUrl: local ? "http://127.0.0.1:8182" : "https://factoriosurvivalworld.com",
  chatUrl: "https://chat.factoriosurvivalworld.com/accounts/login/social/oidc/keycloak",
  playUrl: "https://game.errantry.renownandruin.com/",
};

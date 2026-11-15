// Where the site and its login provider live. Read once, in the browser.
const local = ["127.0.0.1", "localhost"].includes(location.hostname);

export const CONFIG = {
  issuer: local
    ? "http://127.0.0.1:8181/realms/players"
    : "https://auth.errantry.renownandruin.com/realms/players",
  clientId: "site",
  siteUrl: local ? "http://127.0.0.1:8182" : "https://www.errantry.renownandruin.com",
  chatUrl: "https://chat.errantry.renownandruin.com/accounts/login/social/oidc/keycloak",
  playUrl: "https://game.errantry.renownandruin.com/",
};

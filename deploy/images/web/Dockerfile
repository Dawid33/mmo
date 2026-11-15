# The browser bundle's image (mmo/web in ECR): the static site `just web-release`
# builds into target/web/, behind nginx on 8080. Built from a staging directory
# holding `web/` (a copy of target/web), nginx.conf and this file
# (scripts/orion_world_push.sh).
# nginx.conf is an env template: the image's entrypoint substitutes ${LOGIN_UPSTREAM}
# (e.g. http://login.login.svc.cluster.local) at start and a pod must set it.
FROM nginxinc/nginx-unprivileged:1.29-alpine
COPY nginx.conf /etc/nginx/templates/default.conf.template
COPY web/ /usr/share/nginx/html/

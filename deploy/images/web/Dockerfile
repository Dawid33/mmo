# The browser bundle's image (mmo/web in ECR): the static site `just web-release`
# builds into target/web/, behind nginx on 8080. Built from a staging directory
# holding `web/` (a copy of target/web), nginx.conf and this file
# (scripts/orion_world_push.sh).
# nginx.conf is an env template: the image's entrypoint substitutes ${LOGIN_UPSTREAM}
# (a full name, e.g. http://login.login.svc.cluster.local) at start and a pod must set
# it; NGINX_ENTRYPOINT_LOCAL_RESOLVERS makes the entrypoint export the pod's DNS server
# as ${NGINX_LOCAL_RESOLVERS} for the template's `resolver`. ${MAP_UPSTREAM} / ${MAP_WORLD} (the
# world map's tiles, deploy/modules/world/web.tf) have defaults that serve no map: an image rolled out
# before the apply that sets them still starts (envsubst leaves an unset variable as text, which
# nginx refuses).
FROM nginxinc/nginx-unprivileged:1.29-alpine
ENV NGINX_ENTRYPOINT_LOCAL_RESOLVERS=1
ENV MAP_UPSTREAM=http://127.0.0.1:9 MAP_WORLD=no-map
COPY nginx.conf /etc/nginx/templates/default.conf.template
COPY web/ /usr/share/nginx/html/

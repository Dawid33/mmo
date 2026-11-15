# The browser bundle's image (mmo/web in ECR): the static site `just web-release`
# builds into target/web/, behind nginx on 8080. Built from a staging directory
# holding `web/` (a copy of target/web), nginx.conf and this file
# (scripts/orion_world_push.sh).
# nginx.conf is an env template: the image's entrypoint substitutes ${LOGIN_UPSTREAM}
# (a full name, e.g. http://login.login.svc.cluster.local) at start and a pod must set
# it; NGINX_ENTRYPOINT_LOCAL_RESOLVERS makes the entrypoint export the pod's DNS server
# as ${NGINX_LOCAL_RESOLVERS} for the template's `resolver`.
FROM nginxinc/nginx-unprivileged:1.29-alpine
ENV NGINX_ENTRYPOINT_LOCAL_RESOLVERS=1
COPY nginx.conf /etc/nginx/templates/default.conf.template
COPY web/ /usr/share/nginx/html/

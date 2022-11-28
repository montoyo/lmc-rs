FROM alpine

WORKDIR /workdir
COPY ./ca.pem ./
COPY ./server.pem ./
COPY ./server_private.pem ./
COPY ./mosquitto.conf ./
RUN apk add --no-cache mosquitto

EXPOSE 1883 8883
ENTRYPOINT ["mosquitto", "-c", "./mosquitto.conf"]

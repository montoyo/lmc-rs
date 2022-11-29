FROM alpine

WORKDIR /workdir
RUN apk add --no-cache mosquitto
COPY ./ca.pem ./
COPY ./server.pem ./
COPY ./server_private.pem ./
COPY ./mosquitto.conf ./

EXPOSE 1883 8883
ENTRYPOINT ["mosquitto", "-c", "./mosquitto.conf"]

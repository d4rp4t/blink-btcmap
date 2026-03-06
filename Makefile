.PHONY: update-vendor write-sdl generate-supergraph start-deps stop-deps start dev

update-vendor:
	vendir sync

write-sdl:
	cargo run --bin write_sdl > subgraph/schema.graphql

generate-supergraph:
	rover supergraph compose \
		--config dev/apollo-federation/supergraph-config.yaml \
		> dev/apollo-federation/supergraph.graphql

start-deps:
	docker compose \
		-f vendor/blink-quickstart/docker-compose.yml \
		-f docker-compose.yml \
		up -d

stop-deps:
	docker compose \
		-f vendor/blink-quickstart/docker-compose.yml \
		-f docker-compose.yml \
		down -t 3

start:
	BTCMAP_API_KEY=$${BTCMAP_API_KEY} cargo run

dev:
	BTCMAP_API_KEY=$${BTCMAP_API_KEY} cargo watch -x run

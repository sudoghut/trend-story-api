# trend-story-api

## Building with Docker

You can build a release binary within a Fedora Docker container:

1.  **Build Image:** `docker build -t trend-story-api .`
2.  **Create Container:** `docker create --name trend-story-api-container trend-story-api`
3.  **Copy Binary:** `docker cp trend-story-api-container:/app/target/release/trend-story-api .`
4.  **Cleanup (Optional):** `docker rm trend-story-api-container`
5.  **Remove Image (Optional):** `docker rmi trend-story-api`
6.  **Remove Build Cache (Optional):** `docker builder prune`
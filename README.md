# trend-story-api

## Building with Docker

You can build a release binary within a Fedora Docker container:

1.  **Build Image:** `docker build -t trend-story-api .`
2.  **Create Container:** `docker create --name trend-story-api-container trend-story-api`
3.  **Copy Binary:** `docker cp trend-story-api-container:/app/target/release/trend-story-api .`
4.  **Cleanup (Optional):** `docker rm trend-story-api-container`
5.  **Remove Image (Optional):** `docker rmi trend-story-api`
6.  **Remove Build Cache (Optional):** `docker builder prune`

## Installation on Linux

1. Clone the repository:
    ```bash
    git clone https://github.com/sudoghut/trend-story-api
    ```

2.  **Create a systemd Unit File:**
    Create a file named `trend-story-api.service` in `/etc/systemd/system/` with the following content. **Change the following values** for your actual settings.

    ```ini
    [Unit]
    Description=Trend Story API Server
    After=network.target

    [Service]
    User=linuxuser
    Group=linuxuser
    WorkingDirectory=/home/linuxuser/trend-story-api
    ExecStart=/usr/bin/env /home/linuxuser/trend-story-api/trend-story-api
    Restart=on-failure
    StandardOutput=journal
    StandardError=journal

    [Install]
    WantedBy=multi-user.target
    ```

3.  **Enable and Start the Service:**
    ```bash
    # Reload systemd to recognize the new service file
    sudo systemctl daemon-reload

    # Enable the service to start on boot
    sudo systemctl enable trend-story-api.service

    # Start the service immediately
    sudo systemctl start trend-story-api.service
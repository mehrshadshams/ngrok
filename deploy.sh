#!/bin/bash
set -e

# Configuration
RESOURCE_GROUP="rg-ngrok"
LOCATION="westus"
DEPLOYMENT_NAME="ngrok-deployment"
BICEP_FILE="infra/main.bicep"

# ACR Details (You should set these or ensure you are logged in)
# We assume you have the credentials available
# Use: ./deploy.sh <image> <acr_server> <acr_username> <acr_password>

if [ "$#" -lt 2 ]; then
    echo "Usage: ./deploy.sh <image> <acr_server> <acr_username> <acr_password>"
    echo "Example: ./deploy.sh mehrshad.azurecr.io/ngrok-server:latest mehrshad.azurecr.io myuser mypassword"
    exit 1
fi

IMAGE=$1
ACR_SERVER=$2
# ACR_USERNAME=$3
# ACR_PASSWORD=$4

# Create Resource Group
echo "Creating resource group $RESOURCE_GROUP in $LOCATION..."
az group create --name $RESOURCE_GROUP --location $LOCATION

# Deploy Bicep
echo "Deploying to Azure Container Instances..."
az deployment group create \
  --name $DEPLOYMENT_NAME \
  --resource-group $RESOURCE_GROUP \
  --template-file $BICEP_FILE \
  --parameters \
    image=$IMAGE \
    acrServer=$ACR_SERVER
    # acrUsername=$ACR_USERNAME \
    # acrPassword=$ACR_PASSWORD

echo "Deployment finished!"

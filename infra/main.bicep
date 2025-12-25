@description('Name of the container group')
param name string = 'ngrok-server'

@description('Location for all resources')
param location string = resourceGroup().location

@description('Container image to deploy. Example: mehrshad.azurecr.io/ngrok-server:latest')
param image string

@description('ACR Server. Example: mehrshad.azurecr.io')
param acrServer string

// @description('ACR Username')
// @secure()
// param acrUsername string

// @description('ACR Password')
// @secure()
// param acrPassword string

@description('Port for the public HTTP web requests')
param publicPort int = 8081

@description('Port for the agent control connection')
param controlPort int = 8089

@description('CPU cores')
param cpuCores int = 1

@description('Memory in GB')
param memoryInGb int = 2

resource acrIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-identity'
  location: location
}

resource acr 'Microsoft.ContainerRegistry/registries@2023-01-01-preview' ={
  name: 'mehrshadacr'
  location: location
  sku: {
    name: 'Standard'
  }
}

// 3. Assign the 'AcrPull' role to the Identity
// AcrPull Role ID: 7f951dda-4ed3-4680-a7ca-43fe172d538d
var acrPullRoleDefinitionId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '7f951dda-4ed3-4680-a7ca-43fe172d538d')

resource roleAssignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  // Use a GUID based on the inputs to ensure the name is unique but deterministic
  name: guid(acr.id, acrIdentity.id, 'AcrPull') 
  scope: acr
  properties: {
    roleDefinitionId: acrPullRoleDefinitionId
    principalId: acrIdentity.properties.principalId
    principalType: 'ServicePrincipal'
  }
}

resource containerGroup 'Microsoft.ContainerInstance/containerGroups@2021-10-01' = {
  name: name
  location: location
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: {
      '${acrIdentity.id}': {}
    }
  }

  properties: {
    sku: 'Standard'
    containers: [
      {
        name: name
        properties: {
          image: image
          ports: [
            {
              port: publicPort
              protocol: 'TCP'
            }
            {
              port: controlPort
              protocol: 'TCP'
            }
          ]
          resources: {
            requests: {
              cpu: cpuCores
              memoryInGB: memoryInGb
            }
          }
        }
      }
    ]
    osType: 'Linux'
    ipAddress: {
      type: 'Public'
      ports: [
        {
          port: publicPort
          protocol: 'TCP'
        }
        {
          port: controlPort
          protocol: 'TCP'
        }
      ]
      dnsNameLabel: name
    }
    imageRegistryCredentials: [
      {
        server: acrServer
        identity: acrIdentity.id 
      }
    ]
    restartPolicy: 'Always'
  }
}

output fqdn string = containerGroup.properties.ipAddress.fqdn
output publicIp string = containerGroup.properties.ipAddress.ip

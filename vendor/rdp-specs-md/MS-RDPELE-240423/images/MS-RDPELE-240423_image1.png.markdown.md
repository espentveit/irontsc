# Remote Desktop Licensing Architecture Diagram

This diagram illustrates the licensing architecture for a Remote Desktop environment, showing how client devices, terminal servers, license servers, and directory services interact to manage and enforce software licenses.

## Core Components

The architecture consists of the following main components:

- **Remote Desktop Client**
- **Terminal Server**
- **Active Directory**
- **License Server**
- **License Manager**
- **Clearing House**

## Component Details and Relationships

### 1. Remote Desktop Client
- **License Type**: Per Device License
- **Connection to Terminal Server**: Uses RDP (Remote Desktop Protocol)
- **Connection to License Server**: Uses RPC (Remote Procedure Call)

### 2. Terminal Server
- **Connection to Active Directory**: Uses LDAP (Lightweight Directory Access Protocol)
- **Connection to License Server**: Uses RPC
- **Connection to Remote Desktop Client**: Uses RDP

### 3. Active Directory
- **License Type**: Per User License
- **Connection to Terminal Server**: Uses LDAP
- **Connection to License Server**: Uses LDAP

### 4. License Server
- **Contains**: License DB (License Database)
- **Connection to Terminal Server**: Uses RPC
- **Connection to License Manager**: Uses RPC
- **Connection to Active Directory**: Uses LDAP

### 5. License Manager
- **Connection to License Server**: Uses RPC
- **Connection to Clearing House**: Uses Https/Web/Telephone

### 6. Clearing House
- **Connection to License Manager**: Via Https/Web/Telephone

## Summary of Interactions

| Component            | Connects To             | Protocol/Method      | Purpose                                      |
|----------------------|-------------------------|----------------------|----------------------------------------------|
| Remote Desktop Client| Terminal Server         | RDP                  | Establishes remote desktop session           |
| Remote Desktop Client| License Server          | RPC                  | Requests license validation                  |
| Terminal Server      | Active Directory        | LDAP                 | Authenticates users                          |
| Terminal Server      | License Server          | RPC                  | Requests license for session                 |
| Terminal Server      | Active Directory        | LDAP                 | Queries user license status                  |
| License Server       | Active Directory        | LDAP                 | Syncs user license data                      |
| License Server       | License Manager         | RPC                  | Receives license management commands         |
| License Manager      | Clearing House          | Https/Web/Telephone  | Reports license usage or obtains updates     |
| License Manager

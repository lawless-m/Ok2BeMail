# Azure AD App Registration Setup

## Overview

To access Microsoft Graph API, you need to register an application in Azure Active Directory (now called Microsoft Entra ID). Since you have admin access, this is straightforward.

## Steps

### 1. Access Azure Portal

Navigate to: https://portal.azure.com

### 2. Go to App Registrations

- Search for "App registrations" in the top search bar, or
- Navigate: Azure Active Directory → App registrations

### 3. Create New Registration

Click **"New registration"**

**Name**: `Email Classifier` (or whatever you prefer)

**Supported account types**: 
- Select "Accounts in this organizational directory only" (single tenant)
- This restricts the app to your organisation's tenant

**Redirect URI**:
- Platform: "Public client/native (mobile & desktop)"
- URI: `http://localhost` (for device code flow, this isn't actually used but is required)

Click **Register**

### 4. Note the Application Details

From the Overview page, copy:
- **Application (client) ID** → This is your `AZURE_CLIENT_ID`
- **Directory (tenant) ID** → This is your `AZURE_TENANT_ID`

### 5. Configure API Permissions

Navigate to: **API permissions** (left sidebar)

Click **"Add a permission"**

Select **"Microsoft Graph"**

Select **"Delegated permissions"**

Search and add:
- `Mail.Read` - Read user mail
- `offline_access` - Maintain access (for refresh tokens)

Click **"Add permissions"**

### 6. Grant Admin Consent

Still on the API permissions page:

Click **"Grant admin consent for [Your Organisation]"**

Confirm when prompted.

You should see green checkmarks next to all permissions indicating consent is granted.

### 7. Enable Public Client Flows

Navigate to: **Authentication** (left sidebar)

Scroll to **"Advanced settings"**

Set **"Allow public client flows"** to **Yes**

Click **Save**

This enables the device code flow which doesn't require a client secret.

### 8. (Optional) Client Secret

If you later want to use client credentials flow (daemon without user interaction):

Navigate to: **Certificates & secrets** (left sidebar)

Click **"New client secret"**

Add a description, select expiry, click **Add**

**Copy the secret value immediately** - you can't see it again.

Note: For reading your own mailbox with delegated permissions, you don't need a client secret. Device code flow works without one.

## Configuration

Add to your config file or environment:

```toml
[azure]
client_id = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
tenant_id = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
```

Or as environment variables:
```bash
export AZURE_CLIENT_ID="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
export AZURE_TENANT_ID="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
```

## Testing the Setup

Once the app runs `emailcl init`, it will:

1. Display a URL and a code
2. You open the URL in a browser
3. Enter the code
4. Sign in with your Microsoft account
5. Grant permissions (already admin-consented, so this is quick)
6. App receives tokens and stores them

## Troubleshooting

### "AADSTS50011: The redirect URI specified in the request does not match"

- Check that `http://localhost` is added as a redirect URI
- Ensure "Allow public client flows" is enabled

### "AADSTS65001: The user or administrator has not consented"

- Go back to API permissions and click "Grant admin consent"

### "AADSTS7000218: The request body must contain the following parameter: 'client_assertion' or 'client_secret'"

- Enable "Allow public client flows" in Authentication settings

### Tokens expire after 1 hour

- This is normal. The app should use the refresh token to get new access tokens automatically.
- Refresh tokens last 90 days by default (can be configured in Azure AD).

## Security Notes

- The app registration doesn't contain any secrets by default (device code flow)
- Tokens are stored locally on your machine
- Only you can authenticate to access your mailbox
- If you add a client secret, treat it like a password

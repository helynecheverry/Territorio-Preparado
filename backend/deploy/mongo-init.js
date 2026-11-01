// Only runs for an empty Mongo volume. Passwords are injected by Compose.
const app = db.getSiblingDB('territorio');
app.createUser({user: 'territorio', pwd: process.env.TP_DB_PASSWORD, roles: [{role: 'readWrite', db: 'territorio'}]});
